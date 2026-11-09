// OpenExec dataflow bridge — ported from idtx-flow's ExecBridge/ExecBridgeManager
// (shared/include/idtxflow/exec/ExecBridgeManager.h), adapted for idtx-studio:
//
//  * Godot result handlers (IExecBridgeHandler / ExecComputeResult) are REMOVED;
//    ComputeAndDispatch instead authors each changed computed VtValue directly
//    onto its destination attribute, which idtx-studio's change-notice → hydrate
//    path renders. See `idtx_exec::tick`.
//  * The 100 ms worker thread (ExecBridgeManager::Start/Cancel) is REMOVED;
//    idtx-studio drives `tick()` from its main-thread session tick under the
//    exclusive stage-lifecycle lock, so there is never a second thread mutating
//    the stage (OpenUSD forbids concurrent stage mutation).
//
// Everything here takes raw USD types and authors to the stage's CURRENT edit
// target; the lock + session UsdEditContext + observer checkpoint/rollback live
// in the `exec_*` FFI wrappers in usd_shim.cpp.

#ifdef IDTX_OPENEXEC

#include "exec_compute.h"

#include <atomic>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <map>
#include <mutex>
#include <unordered_map>
#include <utility>
#include <vector>

#include <pxr/usd/usd/attribute.h>
#include <pxr/usd/usd/prim.h>
#include <pxr/usd/usd/primRange.h>
#include <pxr/usd/usd/timeCode.h>
#include <pxr/usd/usdGeom/xformable.h>
#include <pxr/usd/sdf/path.h>
#include <pxr/base/tf/token.h>
#include <pxr/base/vt/value.h>

#include <pxr/exec/execUsd/system.h>
#include <pxr/exec/execUsd/valueKey.h>
#include <pxr/exec/execUsd/request.h>
#include <pxr/exec/execUsd/cacheView.h>

PXR_NAMESPACE_USING_DIRECTIVE

namespace idtx_exec {
namespace {

// USD type names (schemaIdentifier, as reported by UsdPrim::GetTypeName) of the
// IDTX datasource schemas; matches crates/idtx-usd-exec/schema/schema.usda.
constexpr const char* kRestDatasourceType = "RestDatasource";
constexpr const char* kMockDatasourceType = "MockDatasource_RandomFloat";
// Terminal consumer schema (crates/idtx-usd-exec). Matched by type-name string
// rather than `IsA<IDTXComputeValueSink>()` for the same reason the datasources
// above are: the schema's TfType + `_GetStaticTfType()` live in the separate
// idtx_usd_exec plugin DLL, not in this cxx bridge's link unit, so `IsA<>` would
// fail to link here. `GetTypeName()` reads the authored typeName — no symbol needed.
constexpr const char* kComputeValueSinkType = "ComputeValueSink";
// Type name of the host-state / time environment provider schema
// (crates/idtx-usd-exec: Compute_Environment). A graph containing one resolves
// time-varying `idcm:` values (CURRENT_TIME/…) and env/auth tokens that change
// between frames independent of any datasource response — so such a graph must
// recompute every tick. A graph WITHOUT one is purely datasource-driven: its
// computed values only change when a REST/mock payload arrives, so the host can
// skip the compute tick (and never take the exclusive stage lock) on frames with
// no pending payload. See `ExecBridge::has_live_providers_` and `Tick`.
constexpr const char* kComputeEnvironmentType = "Compute_Environment";
// Type-name prefix of the codeless 2D-widget schemas (idtx-usd-extension:
// IdtxWidgetPanel / IdtxWidgetLabel / IdtxWidgetSlider / …). A widget element
// prim (e.g. IdtxWidgetLabel : UsdTyped) is NOT xformable, so without this it
// would be skipped by the value-key traversal and a connected widget value
// attribute (`idtx:widget:value.connect = </…/Compute_*.outputs:…>`) would
// never become a compute leaf. Matched by type-name string for the same
// link-unit reason as kComputeValueSinkType above (the TfType lives in the
// separate idtx_usd_extension plugin). The IsA/GetTypeName widening is safe:
// RegisterAttributeWithConnection still only registers connections whose
// *source* is a Compute_* node, so a widget prim with no compute binding (or a
// connection to a non-compute prim) registers nothing. See
// docs/widgets-in-usd-proposal-v2.md §3.
constexpr const char* kWidgetTypePrefix = "IdtxWidget";

// The base-Datasource output attribute the external payload is written to.
const TfToken kOutputsData("outputs:data");

enum class Lifecycle { Created, Active, Stopped };

// Opt-in first-tick tracing of the compute path (`IDTX_EXEC_TRACE` in the env).
// Off by default; it isolates which value key / attribute trips a type fatal on a
// new platform. Evaluated once.
inline bool exec_trace() {
    static const bool on = std::getenv("IDTX_EXEC_TRACE") != nullptr;
    return on;
}

struct StageAttributeUpdate {
    SdfPath prim_path;
    TfToken attribute_name;
    VtValue value;
};

// Parallel-array metadata mapping an OpenExec value-key index back to the stage
// attribute whose computed result it carries (OpenExec's CacheView is index-only).
struct ValueKeyMetadata {
    SdfPath attributePath;       // full path of the attribute to author
    TfToken computationName;     // the computation driving it (for logging)
    VtValue lastComputedValue;   // change-detection: skip re-authoring if equal
    // When true (the default — scene consumers like xformables / ComputeValueSink)
    // Tick authors the computed value onto `attributePath`. When false (a
    // datasource INPUT such as a connected `authorization` / `jsonBody`) Tick does
    // NOT author to the stage: the resolved value is cached in `lastComputedValue`
    // and handed to the host poller via `data_source_inputs()` instead. This keeps
    // a resolved bearer token out of the session layer while still recomputing it
    // every tick through the normal time-dependency path.
    bool authorBack = true;
};

// One per stage. Holds the OpenExec system/request, the registered value keys +
// their metadata, and the coalescing pending-update queue the REST poller feeds.
class ExecBridge {
  public:
    explicit ExecBridge(const UsdStageRefPtr& stage)
        : stage_(stage), exec_system_(std::make_unique<ExecUsdSystem>(stage)) {}

    std::size_t GetValueKeyCount() const { return exec_value_keys_.size(); }
    Lifecycle lifecycle() const { return lifecycle_.load(std::memory_order_acquire); }

    // Mark that this stage carries a time/host-state environment provider
    // (Compute_Environment). Set once at build; makes every tick recompute.
    void NoteLiveProvider() { has_live_providers_ = true; }
    bool HasLiveProviders() const { return has_live_providers_; }

    // Register an attribute carrying an authored connection as an OpenExec value
    // key. The first connection target's prim + property-name (the computation
    // token) are resolved; returns the value-key index, or -1 if unresolved.
    //
    // `authorBack` selects what `Tick` does with the computed result: scene
    // consumers (xformables / ComputeValueSink) pass `true` so the value is
    // authored onto the attribute; datasource INPUTS (connected `authorization` /
    // `jsonBody`) pass `false` so the value is cached for the poller instead of
    // written to the stage (see `ValueKeyMetadata::authorBack`).
    int RegisterAttributeWithConnection(const UsdAttribute& attribute, bool authorBack) {
        SdfPathVector connections;
        if (!attribute.GetConnections(&connections) || connections.empty()) return -1;
        const SdfPath& connection = connections[0];
        const SdfPath primPath = connection.GetPrimPath();
        const std::string propName = connection.GetName();
        if (primPath.IsEmpty() || propName.empty()) return -1;
        const UsdPrim sourcePrim = stage_->GetPrimAtPath(primPath);
        if (!sourcePrim.IsValid()) return -1;
        // Only connections whose provider is one of the plugin's compute schemas
        // (type name "Compute_*") carry an OpenExec computation. A normal USD
        // attribute connection on an xformable that points at any other prim has
        // no computation of that property name, so registering it as a value key
        // makes BuildRequest fail for the WHOLE stage ("Failed to find
        // computation '<name>'"), taking every real compute chain down with it.
        // The terminal consumer of a chain always connects to a Compute_* node
        // (datasources feed compute nodes, not xformables directly), so this
        // filter never drops a valid key.
        if (sourcePrim.GetTypeName().GetString().rfind("Compute_", 0) != 0) return -1;

        const TfToken computationToken(propName);
        const int index = static_cast<int>(exec_value_keys_.size());
        if (exec_trace()) {
            std::fprintf(stderr, "[idtx-exec] register key i=%d attr=%s -> src=%s comp=%s author_back=%d\n",
                         index, attribute.GetPath().GetText(), primPath.GetText(),
                         computationToken.GetText(), authorBack ? 1 : 0);
        }
        exec_value_keys_.emplace_back(sourcePrim, computationToken);
        value_key_metas_.push_back({attribute.GetPath(), computationToken, VtValue(), authorBack});
        return index;
    }

    // Collect the latest resolved values of every datasource-INPUT value key
    // (those registered with `authorBack=false`) that currently holds a string.
    // Called after `Tick` to hand the host poller its live `authorization` /
    // `jsonBody` without any stage authoring. See `data_source_inputs`.
    void CollectDatasourceInputs(std::vector<DataSourceInput>& out) const {
        for (const ValueKeyMetadata& md : value_key_metas_) {
            if (md.authorBack) continue;
            if (!md.lastComputedValue.IsHolding<std::string>()) continue;
            DataSourceInput in;
            in.prim_path = md.attributePath.GetPrimPath().GetString();
            in.attribute_name = md.attributePath.GetName();
            in.value = md.lastComputedValue.Get<std::string>();
            out.push_back(std::move(in));
        }
    }

    // Compile the request for all registered value keys and go Active. Consumes
    // exec_value_keys_ by move — no more registration after this (rebuild the
    // whole bridge on topology change).
    void BuildAndActivate() {
        if (exec_value_keys_.empty()) return;
        auto invalidation_cb = [](const ExecRequestIndexSet& /*indices*/,
                                  const EfTimeInterval& /*interval*/) {};
        if (exec_trace()) {
            std::fprintf(stderr, "[idtx-exec] BuildRequest: %zu value key(s)\n",
                         exec_value_keys_.size());
        }
        exec_request_ = std::make_unique<ExecUsdRequest>(
            exec_system_->BuildRequest(std::move(exec_value_keys_), invalidation_cb));
        if (exec_trace()) std::fprintf(stderr, "[idtx-exec] PrepareRequest\n");
        exec_system_->PrepareRequest(*exec_request_);
        if (exec_trace()) std::fprintf(stderr, "[idtx-exec] request active\n");
        lifecycle_.store(Lifecycle::Active, std::memory_order_release);
    }

    // Coalescing enqueue of an external payload. Thread-safe; no stage access.
    bool Enqueue(StageAttributeUpdate update) {
        if (lifecycle() != Lifecycle::Active || update.prim_path.IsEmpty() ||
            update.attribute_name.IsEmpty() || update.value.IsEmpty()) {
            return false;
        }
        const std::string key =
            update.prim_path.GetString() + "." + update.attribute_name.GetString();
        std::lock_guard<std::mutex> lock(queue_mutex_);
        auto it = pending_updates_.find(key);
        if (it != pending_updates_.end()) {
            it->second = std::move(update);
            return true;
        }
        if (pending_updates_.size() >= kMaxPendingUpdates) return false;
        pending_update_order_.push_back(key);
        pending_updates_.emplace(key, std::move(update));
        return true;
    }

    // Apply pending updates + Compute + author changed results onto the current
    // edit target. Returns the number of attributes authored this tick.
    std::size_t Tick() {
        if (lifecycle() != Lifecycle::Active) return 0;
        const bool trace = !traced_first_tick_ && exec_trace();
        if (trace) std::fprintf(stderr, "[idtx-exec] tick: begin (apply pending)\n");
        // Applying a pending payload authors the datasource's `outputs:data`,
        // which is a stage mutation that emits observer notices the host must
        // drain. Seed the authored count with those writes so a tick that only
        // refreshes payloads — while the downstream computed consumer value
        // happens to stay equal — still returns non-zero and triggers the host
        // drain. Counting only consumer writes let those notices accumulate
        // indefinitely whenever an input changed but the result did not.
        std::size_t authored = ApplyPendingUpdates();
        if (!exec_request_ || !exec_request_->IsValid()) return authored;

        // Idle fast-exit: when no payload was applied this tick AND the graph has
        // no time/host-state provider, nothing downstream can have changed, so skip
        // the (potentially expensive) ChangeTime + Compute + value-key read entirely.
        // The host gate (exec_has_work) normally prevents us from being called at
        // all in this case; this is the defensive second line so a direct caller
        // still pays nothing on an idle datasource-only graph.
        if (authored == 0 && !has_live_providers_) return 0;

        // Advance Exec time so time-dependent / host-state providers
        // (Compute_Environment) re-invalidate; the absolute value is irrelevant
        // (providers read host state directly). The earlier on-device abort inside
        // this network walk was the cross-.so type split — fixed by loading the
        // plugin as a DT_NEEDED of the runtime (see idtx-usd-exec/build.rs), so the
        // walk now sees unified types.
        exec_time_frame_ += 1.0;
        if (trace) std::fprintf(stderr, "[idtx-exec] tick: ChangeTime %.0f\n", exec_time_frame_);
        exec_system_->ChangeTime(UsdTimeCode(exec_time_frame_));

        if (trace) std::fprintf(stderr, "[idtx-exec] tick: Compute begin\n");
        ExecUsdCacheView view = exec_system_->Compute(*exec_request_);

        // First-tick breadcrumbs: isolate which value key / attribute trips an
        // OpenExec Vdf type fatal on device (the compute runs once per frame, so
        // only trace the first tick to avoid a 72 Hz log flood). The LAST line
        // printed before an abort names the offending attribute + computation.
        traced_first_tick_ = true;
        if (trace) {
            std::fprintf(stderr, "[idtx-exec] tick: computed view, reading %zu key(s)\n",
                         value_key_metas_.size());
        }

        // Add each changed consumer write on top of the datasource payload
        // writes already counted by ApplyPendingUpdates above.
        const std::size_t count = value_key_metas_.size();
        for (std::size_t i = 0; i < count; ++i) {
            ValueKeyMetadata& md = value_key_metas_[i];
            if (trace) {
                std::fprintf(stderr, "[idtx-exec] tick: get key i=%zu attr=%s comp=%s\n",
                             i, md.attributePath.GetText(), md.computationName.GetText());
            }
            VtValue computedValue = view.Get(static_cast<int>(i));
            if (trace) {
                std::fprintf(stderr, "[idtx-exec] tick:   got key i=%zu type=%s\n",
                             i, computedValue.GetTypeName().c_str());
            }
            if (computedValue == md.lastComputedValue) continue;  // change-skip

            // Datasource INPUT (connected authorization / jsonBody): cache the
            // freshly-resolved value for the host poller (CollectDatasourceInputs)
            // and do NOT author it onto the stage — a resolved bearer token must
            // not land in the session layer, and this value drives an off-stage
            // HTTP header, not rendered state, so it is not an `authored` change
            // (no reprojection). The recompute itself still happened through the
            // normal time-dependency path, so the token stays fresh.
            if (!md.authorBack) {
                md.lastComputedValue = computedValue;
                continue;
            }

            const UsdPrim prim = stage_->GetPrimAtPath(md.attributePath.GetPrimPath());
            UsdAttribute attr =
                prim ? prim.GetAttribute(md.attributePath.GetNameToken()) : UsdAttribute();
            // Commit the cached value ONLY after the session-layer write succeeds.
            // Caching before the Set would make a missing attribute or a transient
            // type failure look "unchanged" forever, suppressing every retry until
            // the computation itself produces a different value.
            if (attr && attr.Set(computedValue)) {
                md.lastComputedValue = computedValue;
                ++authored;
            }
        }
        return authored;
    }

  private:
    // Author every coalesced pending payload onto its datasource's output
    // attribute. Returns the number of attributes successfully Set — each is a
    // stage mutation the host must drain (see `Tick`).
    std::size_t ApplyPendingUpdates() {
        std::vector<StageAttributeUpdate> updates;
        {
            std::lock_guard<std::mutex> lock(queue_mutex_);
            for (const auto& key : pending_update_order_) {
                auto it = pending_updates_.find(key);
                if (it != pending_updates_.end()) updates.push_back(std::move(it->second));
            }
            pending_updates_.clear();
            pending_update_order_.clear();
        }
        std::size_t applied = 0;
        for (const auto& update : updates) {
            const UsdPrim prim = stage_->GetPrimAtPath(update.prim_path);
            UsdAttribute attribute =
                prim ? prim.GetAttribute(update.attribute_name) : UsdAttribute();
            const TfToken type =
                attribute ? attribute.GetTypeName().GetAsToken() : TfToken();
            const bool compatible =
                attribute &&
                ((type == TfToken("string") && update.value.IsHolding<std::string>()) ||
                 (type == TfToken("float") && update.value.IsHolding<float>()) ||
                 (type == TfToken("double") && update.value.IsHolding<double>()));
            if (!compatible) continue;
            if (attribute.Set(update.value)) ++applied;
        }
        return applied;
    }

    UsdStageRefPtr stage_;
    std::unique_ptr<ExecUsdSystem> exec_system_;
    std::unique_ptr<ExecUsdRequest> exec_request_;
    std::vector<ExecUsdValueKey> exec_value_keys_;
    std::vector<ValueKeyMetadata> value_key_metas_;
    static constexpr std::size_t kMaxPendingUpdates = 1024;
    std::unordered_map<std::string, StageAttributeUpdate> pending_updates_;
    std::vector<std::string> pending_update_order_;
    std::mutex queue_mutex_;
    std::atomic<Lifecycle> lifecycle_{Lifecycle::Created};
    double exec_time_frame_ = 0.0;
    bool traced_first_tick_ = false;  // one-shot first-tick breadcrumbs (device debug)
    // True when the graph carries a Compute_Environment (time/host-state) provider,
    // so every tick must recompute; false for a purely datasource-driven graph,
    // which the host may skip on frames with no pending payload. Set at build.
    bool has_live_providers_ = false;

    ExecBridge(const ExecBridge&) = delete;
    ExecBridge& operator=(const ExecBridge&) = delete;
};

// Process-global per-stage bridge registry (the ExecBridgeManager role, minus
// the worker thread). Keyed by stage; guarded for the rare build/release races.
std::mutex g_registry_mutex;
std::map<UsdStageRefPtr, std::unique_ptr<ExecBridge>> g_bridges;

ExecBridge* find_bridge(const UsdStageRefPtr& stage) {
    std::lock_guard<std::mutex> lock(g_registry_mutex);
    auto it = g_bridges.find(stage);
    return it == g_bridges.end() ? nullptr : it->second.get();
}

std::string read_string_attr(const UsdPrim& prim, const char* name,
                             const std::string& fallback) {
    const UsdAttribute a = prim.GetAttribute(TfToken(name));
    std::string v;
    if (a && a.Get(&v)) return v;
    return fallback;
}

// Read a `token`-typed attribute as a std::string. `method` is authored as a USD
// `token` (schema: `token method = "GET"`), whose stored value is a `TfToken`, so
// `read_string_attr`'s `Get(std::string*)` fails on it and silently returns the
// fallback — which made an authored `method = "POST"` poll as GET (HTTP 405 on an
// endpoint that only accepts POST). Reading it as a `TfToken` and converting fixes
// that.
std::string read_token_attr(const UsdPrim& prim, const char* name,
                            const std::string& fallback) {
    const UsdAttribute a = prim.GetAttribute(TfToken(name));
    TfToken v;
    if (a && a.Get(&v)) return v.GetString();
    return fallback;
}

}  // namespace

std::size_t build_program(const UsdStageRefPtr& stage) {
    if (!stage) return 0;
    auto bridge = std::make_unique<ExecBridge>(stage);
    for (const UsdPrim& prim : stage->Traverse()) {
        // A Compute_Environment provider resolves time-varying / host-state values
        // (idcm: clocks, env/auth tokens) that change independent of any datasource
        // payload, so its presence forces every tick to recompute (disables the
        // host's idle gate for this stage). Noted regardless of the consumer /
        // datasource classification below.
        if (prim.GetTypeName() == TfToken(kComputeEnvironmentType)) {
            bridge->NoteLiveProvider();
        }
        // Scene CONSUMERS carry value keys that are authored back onto the stage:
        // xformable prims (Xform / Gprim / …) and explicit ComputeValueSink prims.
        // A compute node's INPUT connection (`inputs:jsonData.connect =
        // </Src.outputs:data>`) is internal graph wiring resolved by the
        // computation's own `.Connections(...)` declaration, NOT a value key —
        // registering it makes OpenExec 26.08 fail ("Failed to find computation
        // 'outputs:data'" on a datasource, which has no such computation).
        // The terminal consumer of a compute chain is either an xformable
        // prim, an explicit ComputeValueSink (matched by type name — see
        // kComputeValueSinkType above for why not IsA<>), or an `IdtxWidget*`
        // element prim carrying a bound `idtx:widget:value` (kWidgetTypePrefix).
        const bool is_consumer =
            prim.IsA<UsdGeomXformable>() ||
            prim.GetTypeName() == TfToken(kComputeValueSinkType) ||
            prim.GetTypeName().GetString().rfind(kWidgetTypePrefix, 0) == 0;
        // Datasource prims carry INPUT attributes (`authorization`, `jsonBody`)
        // that may themselves be driven by a Compute_* node — e.g.
        // `authorization.connect = </…/GetAuthToken.outputs:value>` resolving a
        // `${auth:TOKEN}` template. Register those as value keys too so they are
        // recomputed every tick (picking up fresh env-provider values through the
        // same time-dependency mechanism the consumers use), but mark them
        // authorBack=false: the resolved value is handed to the host REST poller
        // via `data_source_inputs()` rather than authored onto the stage. The
        // `RegisterAttributeWithConnection` Compute_* source filter still applies,
        // so only inputs actually driven by a compute node are registered; a plain
        // authored `authorization` string carries no connection and is read the
        // old way by `data_sources()`.
        const bool is_datasource = prim.GetTypeName() == TfToken(kRestDatasourceType);
        if (!is_consumer && !is_datasource) continue;
        for (const UsdAttribute& attr : prim.GetAttributes()) {
            if (attr.HasAuthoredConnections()) {
                bridge->RegisterAttributeWithConnection(attr, /*authorBack=*/is_consumer);
            }
        }
    }
    const std::size_t count = bridge->GetValueKeyCount();
    if (count > 0) bridge->BuildAndActivate();

    std::lock_guard<std::mutex> lock(g_registry_mutex);
    if (count > 0) {
        g_bridges[stage] = std::move(bridge);
    } else {
        g_bridges.erase(stage);  // no compute graph: keep the registry empty
    }
    return count;
}

std::vector<DataSourceConfig> data_sources(const UsdStageRefPtr& stage) {
    std::vector<DataSourceConfig> out;
    if (!stage) return out;
    for (const UsdPrim& prim : stage->Traverse()) {
        const TfToken type = prim.GetTypeName();
        const bool is_rest = type == TfToken(kRestDatasourceType);
        const bool is_mock = type == TfToken(kMockDatasourceType);
        if (!is_rest && !is_mock) continue;

        DataSourceConfig cfg;
        cfg.prim_path = prim.GetPath().GetString();
        cfg.kind = is_rest ? "rest" : "mock";
        if (is_rest) {
            cfg.endpoint = read_string_attr(prim, "endpoint", "");
            cfg.query = read_string_attr(prim, "query", "");
            cfg.method = read_token_attr(prim, "method", "GET");
            cfg.authorization = read_string_attr(prim, "authorization", "");
            cfg.json_body = read_string_attr(prim, "jsonBody", "{}");
        }
        float interval = 1.0f;
        if (const UsdAttribute a = prim.GetAttribute(TfToken("interval"))) {
            a.Get(&interval);
        }
        cfg.interval = interval;
        out.push_back(std::move(cfg));
    }
    return out;
}

std::vector<DataSourceInput> data_source_inputs(const UsdStageRefPtr& stage) {
    std::vector<DataSourceInput> out;
    ExecBridge* bridge = find_bridge(stage);
    if (bridge) bridge->CollectDatasourceInputs(out);
    return out;
}

bool enqueue(const UsdStageRefPtr& stage, const std::string& prim_path,
             const std::string& body) {
    ExecBridge* bridge = find_bridge(stage);
    if (!bridge) return false;
    StageAttributeUpdate update;
    update.prim_path = SdfPath(prim_path);
    update.attribute_name = kOutputsData;
    update.value = VtValue(body);
    return bridge->Enqueue(std::move(update));
}

std::size_t tick(const UsdStageRefPtr& stage) {
    ExecBridge* bridge = find_bridge(stage);
    return bridge ? bridge->Tick() : 0;
}

bool has_live_providers(const UsdStageRefPtr& stage) {
    ExecBridge* bridge = find_bridge(stage);
    return bridge ? bridge->HasLiveProviders() : false;
}

void release(const UsdStageRefPtr& stage) {
    std::lock_guard<std::mutex> lock(g_registry_mutex);
    g_bridges.erase(stage);
}

}  // namespace idtx_exec

#endif  // IDTX_OPENEXEC
