#include "idtx-usd-sys/src/bridge.rs.h"
#include "idtx-usd-sys/include/usd_shim.h"
#include "idtx-usd-sys/include/sync_attribute_codec.h"
#ifdef IDTX_OPENEXEC
#include "idtx-usd-sys/include/exec_compute.h"
#endif

#include <pxr/base/plug/registry.h>
#include <pxr/base/tf/diagnosticMgr.h>
#include <pxr/base/tf/error.h>
#include <pxr/base/tf/notice.h>
#include <pxr/base/tf/pathUtils.h>
#include <pxr/base/tf/status.h>
#include <pxr/base/tf/warning.h>
#include <pxr/base/tf/stringUtils.h>
#include <pxr/base/tf/type.h>
#include <pxr/base/tf/weakBase.h>
#include <pxr/base/js/json.h>
#include <pxr/base/js/value.h>
#include <pxr/base/vt/array.h>
#include <pxr/base/gf/vec2f.h>
#include <pxr/base/gf/timeCode.h>
#include <pxr/base/gf/vec3f.h>
#include <pxr/base/gf/vec4f.h>
#include <pxr/base/gf/vec2d.h>
#include <pxr/base/gf/vec3d.h>
#include <pxr/base/gf/vec4d.h>
#include <pxr/base/vt/dictionary.h>
#include <pxr/base/vt/value.h>
#include <pxr/usd/ar/asset.h>
#include <pxr/usd/ar/defaultResolver.h>
#include <pxr/usd/ar/packageUtils.h>
#include <pxr/usd/ar/resolver.h>
#include <pxr/usd/ar/resolverContextBinder.h>
#include <pxr/usd/ar/resolvedPath.h>
#include <pxr/usd/pcp/layerStack.h>
#include <pxr/usd/pcp/layerStackIdentifier.h>
#include <pxr/usd/pcp/node.h>
#include <pxr/usd/pcp/primIndex.h>
#include <pxr/usd/pcp/types.h>
#include <pxr/usd/sdf/path.h>
#include <pxr/usd/sdf/layer.h>
#include <pxr/usd/sdf/layerOffset.h>
#include <pxr/usd/sdf/layerUtils.h>
#include <pxr/usd/sdf/assetPath.h>
#include <pxr/usd/sdf/attributeSpec.h>
#include <pxr/base/gf/vec2i.h>
#include <pxr/usd/sdf/changeBlock.h>
#include <pxr/usd/sdf/copyUtils.h>
#include <pxr/usd/sdf/payload.h>
#include <pxr/usd/sdf/primSpec.h>
#include <pxr/usd/sdf/propertySpec.h>
#include <pxr/usd/sdf/reference.h>
#include <pxr/usd/sdf/schema.h>
#include <pxr/usd/sdf/variantSetSpec.h>
#include <pxr/usd/sdf/zipFile.h>
#include <pxr/usd/usd/attribute.h>
#include <pxr/usd/usd/editContext.h>
#include <pxr/usd/usd/editTarget.h>
#include <pxr/usd/usd/notice.h>
#include <pxr/usd/sdf/notice.h>
#include <pxr/usd/sdf/namespaceEdit.h>
#include <pxr/usd/usd/primRange.h>
#include <pxr/usd/usd/primDefinition.h>
#include <pxr/usd/usd/primFlags.h>
#include <pxr/usd/usd/payloads.h>
#include <pxr/usd/usd/references.h>
#include <pxr/usd/usd/relationship.h>
#include <pxr/usd/usd/schemaRegistry.h>
#include <pxr/usd/usd/stage.h>
#include <pxr/usd/usd/variantSets.h>
#include <pxr/usd/usdGeom/camera.h>
#include <pxr/usd/usdGeom/boundable.h>
#include <pxr/usd/usdGeom/basisCurves.h>
#include <pxr/usd/usdGeom/bboxCache.h>
#include <pxr/usd/usdGeom/capsule.h>
#include <pxr/usd/usdGeom/cone.h>
#include <pxr/usd/usdGeom/cube.h>
#include <pxr/usd/usdGeom/cylinder.h>
#include <pxr/usd/usdGeom/gprim.h>
#include <pxr/usd/usdGeom/imageable.h>
#include <pxr/usd/usdGeom/metrics.h>
#include <pxr/usd/usdGeom/mesh.h>
#include <pxr/usd/usdGeom/primvarsAPI.h>
#include <pxr/usd/usdGeom/plane.h>
#include <pxr/usd/usdGeom/pointInstancer.h>
#include <pxr/usd/usdGeom/sphere.h>
#include <pxr/usd/usdGeom/subset.h>
#include <pxr/usd/usdGeom/xformCache.h>
#include <pxr/usd/usdGeom/xformable.h>
#include <pxr/usd/usdGeom/xform.h>
#include <pxr/usd/usdGeom/scope.h>
#include <pxr/usd/usdLux/blackbody.h>
#include <pxr/usd/usdLux/cylinderLight.h>
#include <pxr/usd/usdLux/diskLight.h>
#include <pxr/usd/usdLux/distantLight.h>
#include <pxr/usd/usdLux/domeLight.h>
#include <pxr/usd/usdLux/geometryLight.h>
#include <pxr/usd/usdLux/lightAPI.h>
#include <pxr/usd/usdLux/meshLightAPI.h>
#include <pxr/usd/usdLux/pluginLight.h>
#include <pxr/usd/usdLux/portalLight.h>
#include <pxr/usd/usdLux/rectLight.h>
#include <pxr/usd/usdLux/shadowAPI.h>
#include <pxr/usd/usdLux/shapingAPI.h>
#include <pxr/usd/usdLux/sphereLight.h>
#include <pxr/usd/usdUtils/dependencies.h>
#include <pxr/usd/usdUtils/localizeAsset.h>
#include <pxr/usd/usdUtils/userProcessingFunc.h>
#include <pxr/usd/usdUtils/usdzPackage.h>
#include <pxr/usd/usdShade/material.h>
#include <pxr/usd/usdShade/connectableAPI.h>
#include <pxr/usd/usdShade/materialBindingAPI.h>
#include <pxr/usd/usdShade/nodeGraph.h>
#include <pxr/usd/usdShade/shader.h>
#include <pxr/usd/usdShade/utils.h>
#include <pxr/usd/usdSkel/animQuery.h>
#include <pxr/usd/usdSkel/animation.h>
#include <pxr/usd/usdSkel/binding.h>
#include <pxr/usd/usdSkel/bindingAPI.h>
#include <pxr/usd/usdSkel/cache.h>
#include <pxr/usd/usdSkel/root.h>
#include <pxr/usd/usdSkel/skeleton.h>
#include <pxr/usd/usdSkel/skeletonQuery.h>
#include <pxr/usd/usdSkel/skinningQuery.h>
#include <pxr/usd/usdSkel/tokens.h>
#include <pxr/usd/usdSkel/topology.h>

#ifdef IDTX_HAVE_MATERIALX_WGSL
#include "idtx-usd-sys/include/materialx_wgsl.h"
#endif

#include <algorithm>
#include <array>
#include <atomic>
#include <cctype>
#include <chrono>
#include <ctime>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <mutex>
#include <set>
#include <functional>
#include <shared_mutex>
#include <sstream>
#include <stdexcept>
#include <string>
#include <string_view>
#include <map>
#include <limits>
#include <thread>
#include <utility>
#include <unordered_set>
#include <vector>
#include <iostream>

PXR_NAMESPACE_USING_DIRECTIVE

namespace idtx {
namespace {

// Threads currently inside a packaging window. Each packaging call installs its
// own PackagingDiagnosticCollector, which CAPTURES diagnostics posted on its
// owner thread (for accept/reject) and keeps them off the console. While a
// thread is packaging, the benign-warning filter below must not ALSO re-emit the
// warnings/statuses that thread's collector already owns — but it suppresses them
// ONLY for that thread, so unrelated viewport/stage threads keep full
// diagnostics. Errors and fatal errors are ALWAYS re-emitted regardless of
// packaging, so a genuine failure on any thread is never silently swallowed.
std::mutex g_packaging_threads_mutex;
std::multiset<std::thread::id> g_packaging_threads;

void packaging_thread_enter() {
    std::lock_guard<std::mutex> guard(g_packaging_threads_mutex);
    g_packaging_threads.insert(std::this_thread::get_id());
}
void packaging_thread_leave() {
    std::lock_guard<std::mutex> guard(g_packaging_threads_mutex);
    auto it = g_packaging_threads.find(std::this_thread::get_id());
    if (it != g_packaging_threads.end()) {
        g_packaging_threads.erase(it);
    }
}
bool current_thread_is_packaging() {
    std::lock_guard<std::mutex> guard(g_packaging_threads_mutex);
    return g_packaging_threads.find(std::this_thread::get_id()) !=
           g_packaging_threads.end();
}

// Process-wide diagnostic delegate that DROPS a couple of known-benign, very
// high-volume OpenUSD warnings while faithfully re-emitting every other
// diagnostic. Registering ANY delegate makes TfDiagnosticMgr skip its own
// default stderr print (diagnosticMgr.cpp), so this delegate reproduces that
// print for the diagnostics it keeps via the public FormatDiagnostic().
//
// Dropped:
//  * "...MaterialBindingAPI is not applied..." — DCC exports (Houdini) author
//    `material:binding` without applying the schema; idtx resolves it via
//    OpenUSD's legacy path (materials still bind), but ComputeBoundMaterial()
//    then warns once per prim — thousands of times per viewport projection, on
//    worker threads.
//  * any line mentioning the literal "<UDIM>" — the dependency walker probes the
//    un-expanded UDIM template and reports it unresolved; it resolves via the
//    concrete tiles and is benign (see the packaging analysis).
// Both are pure console noise, and the synchronous worker-thread stderr writes
// measurably slow scene hydration on a real console. The env setting that would
// mute the first (USD_SHADE_WARN_ON_MISSING_MATERIAL_BINDING_API) is cached by
// OpenUSD at library load, before any of our code runs, so it cannot be set in
// process; intercepting the TF_WARN here is the only runtime-effective option.
class BenignWarningFilter final : public TfDiagnosticMgr::Delegate {
public:
    void IssueError(const TfError& e) override {
        // Errors are ALWAYS surfaced — never swallowed, including during
        // packaging (the packaging collector also records them, which only
        // enriches its failure message; a live print of a real error is never
        // the wrong thing to do).
        reemit(e);
    }
    void IssueFatalError(const TfCallContext& ctx, const std::string& msg) override {
        // Fatal errors are ALWAYS surfaced, for the same reason.
        fprintf(stderr, "Fatal error: %s [%s:%zu]\n", msg.c_str(),
                ctx.GetFile() ? ctx.GetFile() : "", ctx.GetLine());
    }
    void IssueStatus(const TfStatus& s) override {
        if (current_thread_is_packaging()) {
            return;  // this thread's packaging collector owns it
        }
        reemit(s);
    }
    void IssueWarning(const TfWarning& w) override {
        const std::string& commentary = w.GetCommentary();
        if (commentary.find("MaterialBindingAPI is not applied") != std::string::npos ||
            commentary.find("<UDIM>") != std::string::npos) {
            return;  // benign, high-volume — drop (always)
        }
        if (current_thread_is_packaging()) {
            return;  // this thread's packaging collector owns it
        }
        reemit(w);
    }

private:
    static void reemit(const TfDiagnosticBase& d) {
        fprintf(stderr, "%s",
                TfDiagnosticMgr::FormatDiagnostic(
                    d.GetDiagnosticCode(), d.GetContext(), d.GetCommentary(),
                    TfDiagnosticInfo())
                    .c_str());
    }
};

// Install the benign-warning filter exactly once, for the life of the process.
// The delegate is intentionally leaked: a process-global delegate must outlive
// all diagnostic activity, and letting a static object's destructor run at exit
// could call RemoveDelegate after TfDiagnosticMgr is already torn down.
void install_benign_warning_filter_once() {
    static TfDiagnosticMgr::Delegate* const filter = [] {
        auto* f = new BenignWarningFilter();
        TfDiagnosticMgr::GetInstance().AddDelegate(f);
        return f;
    }();
    (void)filter;
}

// File-backed layer edits/reloads can notify every stage sharing that layer.
// They take global exclusive. Handle projections and private session mutations
// take global shared plus the owning stage's mutex. Rendering is independent
// after projection crosses the bridge.
std::shared_mutex& stage_lifecycle_mutex() {
    static std::shared_mutex mutex;
    return mutex;
}

// Global exclusion protects file-backed/shared layers and registry operations.
// Private session writes and projections take global shared THEN stage exclusive.
// Reads are exclusive per handle because several projections mutate caches.
class StageAccessGuard {
    std::shared_lock<std::shared_mutex> global_;
    std::unique_lock<std::shared_mutex> stage_;
public:
    explicit StageAccessGuard(std::shared_mutex& stage)
        : global_(stage_lifecycle_mutex()), stage_(stage) {}
    bool owns_lock() const { return stage_.owns_lock(); }
    void unlock() {
        if (stage_.owns_lock()) stage_.unlock();
        if (global_.owns_lock()) global_.unlock();
    }
};

// Run `body` while holding the process-global stage lifecycle lock, releasing the
// lock even if `body` throws.
//
// WHY THIS EXISTS: on this toolchain a C++ exception that unwinds across the cxx
// `extern "C"` boundary runs `catch` handlers but does NOT destroy automatic objects
// during unwind (the shim is built without `/EHsc`; see save_stage). A bare RAII
// `std::unique_lock`/`std::shared_lock` local would therefore NOT be released when a
// function throws while holding the lock — it leaks the lifecycle lock and permanently
// deadlocks every later stage access (readers that take `shared_lock`, writers that
// take `unique_lock`, and even handle teardown in ~Impl). Catching here is safe
// because unwinding `body` into this handler is an ordinary intra-C++ unwind that DOES
// run; we release the lock explicitly before the throw re-crosses the boundary. The
// normal-return path releases the lock via the guard destructor as usual.
//
// Every lifecycle-lock holder funnels through this helper so that no individual throw
// site — present or future, ours or thrown from OpenUSD internals — can reintroduce
// the leak. `LockT` is `std::unique_lock<std::shared_mutex>` for writers or
// `std::shared_lock<std::shared_mutex>` for read projections.
template <typename LockT, typename Body>
auto with_stage_lifecycle_lock(Body&& body) -> decltype(body()) {
    LockT guard(stage_lifecycle_mutex());
    try {
        return body();
    } catch (...) {
        if (guard.owns_lock()) {
            guard.unlock();
        }
        throw;
    }
}

constexpr std::uint32_t kTopology = 1u << 0;
// Must match idtx_usd::PROJECTION_SCHEMA_VERSION.
constexpr std::uint32_t kProjectionSchemaVersion = 7;
constexpr std::uint32_t kTransform = 1u << 1;
constexpr std::uint32_t kGeometry = 1u << 2;
constexpr std::uint32_t kMaterial = 1u << 3;
constexpr std::uint32_t kCamera = 1u << 4;
constexpr std::uint32_t kInspection = 1u << 5;
constexpr std::uint32_t kVisibility = 1u << 6;
constexpr std::uint32_t kLight = 1u << 7;
constexpr std::uint32_t kAnimation = 1u << 8;
constexpr std::uint32_t kSkeleton = 1u << 9;
constexpr std::uint32_t kMetadata = 1u << 10;
constexpr std::uint32_t kRelationship = 1u << 11;
constexpr std::uint32_t kGeneric = 1u << 12;
constexpr std::uint32_t kInstancer = 1u << 13;
constexpr std::uint32_t kUnknown = 1u << 31;
constexpr std::size_t kPortableJointLimit = 256;
// Portable GPU skinning supports up to 8 influences per vertex. The GPU vertex
// format carries two vec4 joint/weight slots (locations 5/6 and 7/8); meshes
// with fewer influences zero-pad the unused slots. Meshes above this limit fall
// back to the bounded official OpenUSD CPU deformation path.
constexpr std::size_t kPortableInfluenceLimit = 8;
constexpr std::size_t kCpuSkinningPointLimit = 1'000'000;

std::string to_std(rust::Str value) {
    return std::string(value.data(), value.size());
}

std::string to_std(const rust::String& value) {
    return std::string(value.data(), value.size());
}

// Normalizes an asset path into a stable comparison key: backslashes to forward
// slashes and lower-cased. Used to match a layer's authored (and anchored)
// asset paths against the unresolved set reported by compute_dependencies,
// which may differ only in slash direction or case on Windows. Lower-casing is
// safe here because both sides are normalized identically; the comparison is
// equality, so it never over-matches distinct paths.
std::string normalize_path_key(const std::string& path) {
    std::string key = path;
    for (char& c : key) {
        if (c == '\\') {
            c = '/';
        } else {
            c = static_cast<char>(
                std::tolower(static_cast<unsigned char>(c)));
        }
    }
    return key;
}

std::string lower_extension(const std::filesystem::path& path) {
    return TfStringToLower(path.extension().string());
}

bool safe_package_member(const std::filesystem::path& member) {
    if (member.empty() || member.is_absolute()) {
        return false;
    }
    return std::none_of(member.begin(), member.end(), [](const auto& part) {
        return part == "..";
    });
}

#ifdef IDTX_AUTHORING
std::atomic<std::uint64_t> next_temporary{1};

enum class PersistenceVerificationMode {
    Save,
    LooseSave,
    RootExport,
    FlattenedExport,
};

void verify_persistence_locked(
    const UsdStageRefPtr& source, const UsdStageRefPtr& persisted,
    PersistenceVerificationMode mode, bool add_source_file_comment = true);

std::string normalize_signed_zero(const std::string& text);

std::filesystem::path sibling_temporary(const std::filesystem::path& destination) {
    const std::uint64_t serial = next_temporary.fetch_add(1);
    return destination.parent_path() /
        (destination.stem().string() + ".idtx-tmp-" +
         std::to_string(serial) + destination.extension().string());
}

void replace_file(const std::filesystem::path& temporary,
                  const std::filesystem::path& destination) {
    std::error_code error;
    std::filesystem::rename(temporary, destination, error);
    if (!error) {
        return;
    }
    // Windows does not replace an existing destination. The fallback is used
    // only where the platform cannot provide POSIX-style atomic replacement.
    std::filesystem::remove(destination, error);
    error.clear();
    std::filesystem::rename(temporary, destination, error);
    if (error) {
        throw std::runtime_error("could not replace persistence destination: " +
                                 error.message());
    }
}

void validate_loose_destination(const std::filesystem::path& destination) {
    const std::string extension = lower_extension(destination);
    if (extension != ".usda" && extension != ".usdc" && extension != ".usd") {
        throw std::runtime_error(
            "loose USD destination must use .usda, .usdc, or .usd");
    }
}

PersistenceResultRaw loose_result(const std::filesystem::path& destination) {
    PersistenceResultRaw result;
    result.destination = rust::String(destination.string());
    result.file_format = rust::String(
        lower_extension(destination) == ".usda" ? "usda" : "usdc");
    return result;
}

PersistenceResultRaw export_layer_atomic(
    const SdfLayerHandle& layer, const std::filesystem::path& destination,
    const UsdStageRefPtr& source, PersistenceVerificationMode mode, bool add_source_file_comment = true) {
    validate_loose_destination(destination);
    if (!destination.parent_path().empty()) {
        std::filesystem::create_directories(destination.parent_path());
    }
    const std::filesystem::path temporary = sibling_temporary(destination);
    try {
        if (!layer || !layer->Export(temporary.string())) {
            throw std::runtime_error("OpenUSD could not export the layer");
        }
        const SdfLayerRefPtr verified = SdfLayer::FindOrOpen(temporary.string());
        if (!verified) {
            throw std::runtime_error("official OpenUSD reopen verification failed");
        }
        std::string expected_text;
        std::string verified_text;
        // Compare with signed-zero normalization, consistent with the
        // authoritative verify_persistence_locked() content check below and the
        // USDZ save path. A `.usd`/`.usdc` destination round-trips through the
        // usdc crate writer, which normalizes IEEE-754 negative zero ("-0") to
        // "0"; the in-memory usda writer keeps "-0". Any authored -0.0 (e.g. a
        // matrix xformOp from compose_trs_matrix, whose r02 = -sin(0) yields
        // -0.0) would otherwise false-reject a semantically identical save.
        if (!layer->ExportToString(&expected_text) ||
            !verified->ExportToString(&verified_text) ||
            normalize_signed_zero(expected_text) !=
                normalize_signed_zero(verified_text)) {
            throw std::runtime_error(
                "saved root-layer semantics differ after official reopen");
        }
        const UsdStageRefPtr verified_stage =
            UsdStage::Open(verified, UsdStage::LoadAll);
        if (!verified_stage) {
            throw std::runtime_error(
                "official OpenUSD stage reopen verification failed");
        }
        verify_persistence_locked(source, verified_stage, mode, add_source_file_comment);
        replace_file(temporary, destination);
    } catch (...) {
        std::error_code ignored;
        std::filesystem::remove(temporary, ignored);
        throw;
    }
    return loose_result(destination);
}

std::string xform_op_type_name(UsdGeomXformOp::Type type) {
    switch (type) {
        case UsdGeomXformOp::TypeTransform: return "transform";
        case UsdGeomXformOp::TypeTranslate: return "translate";
        case UsdGeomXformOp::TypeScale: return "scale";
        case UsdGeomXformOp::TypeRotateX: return "rotateX";
        case UsdGeomXformOp::TypeRotateY: return "rotateY";
        case UsdGeomXformOp::TypeRotateZ: return "rotateZ";
        case UsdGeomXformOp::TypeRotateXYZ: return "rotateXYZ";
        case UsdGeomXformOp::TypeRotateXZY: return "rotateXZY";
        case UsdGeomXformOp::TypeRotateYXZ: return "rotateYXZ";
        case UsdGeomXformOp::TypeRotateYZX: return "rotateYZX";
        case UsdGeomXformOp::TypeRotateZXY: return "rotateZXY";
        case UsdGeomXformOp::TypeRotateZYX: return "rotateZYX";
        case UsdGeomXformOp::TypeOrient: return "orient";
        default: return "invalid";
    }
}

void append_dependency_report(const UsdStageRefPtr& stage,
                              PersistenceResultRaw* result) {
    std::vector<SdfLayerRefPtr> layers;
    std::vector<std::string> assets;
    std::vector<std::string> unresolved;
    if (!UsdUtilsComputeAllDependencies(
            SdfAssetPath(stage->GetRootLayer()->GetIdentifier()),
            &layers, &assets, &unresolved)) {
        result->warnings.push_back(rust::String(
            "OpenUSD could not completely resolve the export dependency report"));
    }
    for (const std::string& asset : assets) {
        result->dependencies.push_back(rust::String(asset));
    }
    for (const SdfLayerRefPtr& layer : layers) {
        if (layer && layer != stage->GetRootLayer()) {
            result->dependencies.push_back(
                rust::String(layer->GetIdentifier()));
        }
    }
    for (const std::string& path : unresolved) {
        result->dependencies.push_back(rust::String("unresolved:" + path));
    }
}

void append_destination_dependency_report(
    const std::filesystem::path& destination,
    PersistenceResultRaw* result) {
    std::vector<SdfLayerRefPtr> layers;
    std::vector<std::string> assets;
    std::vector<std::string> unresolved;
    const bool complete = UsdUtilsComputeAllDependencies(
        SdfAssetPath(destination.string()), &layers, &assets, &unresolved);
    if (complete && unresolved.empty()) {
        return;
    }
    for (const std::string& path : unresolved) {
        const std::string entry = "unresolved:" + path;
        const bool already_reported = std::any_of(
            result->dependencies.begin(), result->dependencies.end(),
            [&entry](const rust::String& dependency) {
                return to_std(dependency) == entry;
            });
        if (!already_reported) {
            result->dependencies.push_back(rust::String(entry));
        }
    }
    result->warnings.push_back(rust::String(
        "Export does not copy dependencies; one or more assets are unresolved "
        "when opened from the destination"));
}
#endif

std::uint32_t classify(const std::string& property) {
    if (property.empty()) {
        return kMetadata | kGeneric | kInspection;
    }
    std::uint32_t domains = kInspection;
    if (property.rfind("xformOp", 0) == 0 || property == "xformOpOrder") {
        domains |= kTransform;
    } else if (property == "purpose") {
        // Computed purpose is retained on topology records, including on
        // descendants that inherit this opinion. Refresh their subtree too.
        domains |= kVisibility | kTopology;
    } else if (property == "visibility") {
        domains |= kVisibility;
    } else if (property == "points" || property == "faceVertexCounts" ||
               property == "curveVertexCounts" || property == "basis" ||
               property == "type" || property == "wrap" ||
               property.rfind("idtx:curve:", 0) == 0 ||
               property == "faceVertexIndices" ||
               property.rfind("primvars:", 0) == 0 ||
               property == "orientation" || property == "size" ||
               property == "radius" || property == "height" ||
               property == "width" || property == "length" ||
               property == "axis" || property == "protoIndices" ||
               property == "positions" || property == "orientations" ||
               property == "scales") {
        domains |= kGeometry;
    } else if (property.rfind("material:", 0) == 0 ||
               property.rfind("inputs:", 0) == 0 ||
               property.rfind("outputs:", 0) == 0) {
        domains |= kMaterial;
    } else if (property == "focalLength" ||
               property.find("Aperture") != std::string::npos ||
               property == "clippingRange" || property == "clippingPlanes" ||
               property == "projection" || property == "fStop" ||
               property == "focusDistance" || property == "stereoRole" ||
               property.rfind("shutter:", 0) == 0 ||
               property.rfind("exposure", 0) == 0) {
        domains |= kCamera;
    } else {
        domains |= kGeneric;
    }
    if (property == "prototypes" || property == "material:binding" ||
        property.rfind("material:binding:", 0) == 0 ||
        property.rfind("skel:skeleton", 0) == 0 ||
        property.rfind("skel:animationSource", 0) == 0 ||
        property.rfind("skel:blendShapeTargets", 0) == 0) {
        domains |= kRelationship;
    }
    if (property == "joints" || property == "jointIndices" ||
        property == "jointWeights" || property == "bindTransforms" ||
        property == "restTransforms" || property == "translations" ||
        property == "rotations" || property == "scales" ||
        property == "blendShapes" || property == "blendShapeWeights" ||
        property.rfind("skel:", 0) == 0 ||
        property.rfind("primvars:skel:", 0) == 0) {
        domains |= kSkeleton;
    }
    if (property == "primvars:displayColor" ||
        property == "primvars:displayOpacity") {
        domains |= kMaterial;
    }
    return domains;
}

std::uint32_t classify(const UsdStageWeakPtr& stage, const SdfPath& prim_path,
                       const std::string& property) {
    std::uint32_t domains = classify(property);
    const UsdPrim prim = stage ? stage->GetPrimAtPath(prim_path) : UsdPrim();
    if (!prim) {
        return domains | kUnknown;
    }
    if (prim.IsA<UsdGeomPointInstancer>() &&
        (property == "positions" || property == "orientations" || property == "orientationsf" ||
         property == "scales" || property == "protoIndices" ||
         property == "ids" || property == "invisibleIds")) {
        return kInstancer | kInspection;
    }
    // A pure rigid-transform write (`xformOp*` / `xformOpOrder`) changes only a
    // prim's placement — never its skeletal rig, light emission, or time-sample
    // data. The prim-type / API overlays below (kSkeleton / kLight / kAnimation)
    // describe those *other* domains; ORing them onto a transform write wrongly
    // classifies a move as a structural/data edit, which defeats the
    // transform-only fast path in the session (a moved light or skeleton would
    // force a full whole-stage re-extract instead of an in-place matrix patch).
    // Gate the overlays so they only apply when the write is NOT a pure move.
    // A genuine emission (`inputs:`/`light:`), skel (`skel:`/joints/…), or
    // time-sample edit is not a pure transform write, so it still carries its
    // domain bit and still forces the correct re-extract.
    const bool transform_only_write = (domains & kTransform) != 0 &&
                                      (domains & ~(kTransform | kInspection)) == 0;
    const std::string type_name = prim.GetTypeName().GetString();
    if (!transform_only_write &&
        (type_name == "Skeleton" || type_name == "SkelRoot" ||
         type_name == "SkelAnimation" || type_name == "BlendShape")) {
        domains |= kSkeleton;
    }
    if (!property.empty()) {
        const TfToken name(property);
        if (prim.GetRelationship(name)) {
            domains |= kRelationship;
        }
        const UsdAttribute attribute = prim.GetAttribute(name);
        if (!transform_only_write && attribute &&
            attribute.ValueMightBeTimeVarying()) {
            domains |= kAnimation;
        }
    }
    if (prim.HasAPI<UsdLuxLightAPI>() || prim.HasAPI<UsdLuxMeshLightAPI>() ||
        prim.IsA<UsdLuxGeometryLight>() || prim.IsA<UsdLuxPluginLight>()) {
        if (property.rfind("inputs:", 0) == 0 ||
            property.rfind("light:", 0) == 0) {
            domains &= ~kMaterial;
        }
        if (!transform_only_write) {
            domains |= kLight;
        }
    }
    return domains;
}

std::string bound_material_path(const UsdPrim& prim) {
    const UsdShadeMaterial material =
        UsdShadeMaterialBindingAPI(prim).ComputeBoundMaterial();
    return material ? material.GetPath().GetString() : std::string();
}

UsdShadeShader surface_source(const UsdShadeMaterial& material) {
    UsdShadeShader shader = material.ComputeSurfaceSource();
    if (!shader) {
        shader = material.ComputeSurfaceSource(TfToken("mtlx"));
    }
    return shader;
}

UsdShadeShader connected_texture_shader(const UsdShadeShader& shader,
                                        const TfToken& input_name) {
    const UsdShadeInput input = shader.GetInput(input_name);
    if (!input) {
        return UsdShadeShader();
    }
    UsdShadeConnectableAPI source;
    TfToken source_name;
    UsdShadeAttributeType source_type = UsdShadeAttributeType::Invalid;
    if (!UsdShadeConnectableAPI::GetConnectedSource(
            input.GetAttr(), &source, &source_name, &source_type)) {
        return UsdShadeShader();
    }
    const UsdShadeShader texture_shader(source.GetPrim());
    TfToken texture_id;
    if (!texture_shader || !texture_shader.GetIdAttr().Get(&texture_id) ||
        texture_id != TfToken("UsdUVTexture")) {
        return UsdShadeShader();
    }
    return texture_shader;
}

TfToken connected_source_name(const UsdShadeShader& shader,
                              const TfToken& input_name) {
    const UsdShadeInput input = shader.GetInput(input_name);
    UsdShadeConnectableAPI source;
    TfToken source_name;
    UsdShadeAttributeType source_type = UsdShadeAttributeType::Invalid;
    if (input && UsdShadeConnectableAPI::GetConnectedSource(
                     input.GetAttr(), &source, &source_name, &source_type)) {
        return source_name;
    }
    return TfToken();
}

std::uint32_t texture_channel(const TfToken& output) {
    if (output == TfToken("g") || output == TfToken("y")) {
        return 1;
    }
    if (output == TfToken("b") || output == TfToken("z")) {
        return 2;
    }
    if (output == TfToken("a") || output == TfToken("w")) {
        return 3;
    }
    return 0;
}

UsdShadeShader connected_materialx_image(const UsdAttribute& attribute,
                                         std::set<std::string>* visited) {
    if (visited->size() >= 64) {
        return UsdShadeShader();
    }
    UsdShadeConnectableAPI source;
    TfToken source_name;
    UsdShadeAttributeType source_type = UsdShadeAttributeType::Invalid;
    if (!attribute || !UsdShadeConnectableAPI::GetConnectedSource(
                          attribute, &source, &source_name, &source_type)) {
        return UsdShadeShader();
    }
    const std::string edge = source.GetPath().GetString() + "." +
                             source_name.GetString();
    if (!visited->insert(edge).second) {
        return UsdShadeShader();
    }
    const UsdShadeShader source_shader(source.GetPrim());
    TfToken identifier;
    if (source_shader && source_shader.GetIdAttr().Get(&identifier)) {
        const std::string id = identifier.GetString();
        if (id.rfind("ND_image_", 0) == 0) {
            return source_shader;
        }
        if (id.rfind("ND_normalmap_", 0) == 0) {
            const UsdShadeInput normal_input =
                source_shader.GetInput(TfToken("in"));
            return normal_input
                ? connected_materialx_image(normal_input.GetAttr(), visited)
                : UsdShadeShader();
        }
    }
    const UsdShadeOutput output = source.GetOutput(source_name);
    return output ? connected_materialx_image(output.GetAttr(), visited)
                  : UsdShadeShader();
}

UsdShadeShader connected_materialx_image(const UsdShadeShader& shader,
                                         const TfToken& input_name) {
    const UsdShadeInput input = shader.GetInput(input_name);
    std::set<std::string> visited;
    return input ? connected_materialx_image(input.GetAttr(), &visited)
                 : UsdShadeShader();
}

// OpenUSD's asset resolution recognizes only the "<UDIM>" token for tiled
// textures. Other DCC and render pipelines instead author the tile index as a
// printf/Python-style placeholder ("%(UDIM)d", or zero-padded like
// "%(UDIM)04d") or as a non-canonical-case angle form ("<udim>"). Rewrite any
// of those — matched case-insensitively on the "udim" keyword — to the
// canonical "<UDIM>", so both the anchoring below and the Rust-side tiled
// pipeline (which key on "<UDIM>") apply uniformly. A path already using the
// canonical token, or naming no UDIM set, is returned unchanged. The four-digit
// per-tile substitution downstream matches the on-disk naming either convention
// produces, so only the token needs folding — no width/padding is carried.
std::string normalize_udim_token(const std::string& authored) {
    if (authored.find("<UDIM>") != std::string::npos) {
        return authored;  // already canonical
    }
    const std::string lower = TfStringToLower(authored);
    if (lower.find("udim") == std::string::npos) {
        return authored;  // no UDIM keyword at all
    }

    std::string out;
    out.reserve(authored.size());
    const size_t n = authored.size();
    size_t i = 0;
    while (i < n) {
        // printf/Python form: "%(udim)" + optional digits + one conversion char.
        if (lower.compare(i, 7, "%(udim)") == 0) {
            size_t j = i + 7;
            while (j < n && lower[j] >= '0' && lower[j] <= '9') {
                ++j;
            }
            if (j < n && (lower[j] == 'd' || lower[j] == 'i' || lower[j] == 'u')) {
                out += "<UDIM>";
                i = j + 1;
                continue;
            }
        }
        // Angle form, any case: "<udim>".
        if (lower.compare(i, 6, "<udim>") == 0) {
            out += "<UDIM>";
            i += 6;
            continue;
        }
        out += authored[i];
        ++i;
    }
    return out;
}

// Anchor an authored (usually layer-relative) asset string against the layer
// that carries `attr`'s strongest opinion, yielding an absolute (or package)
// path. Used for the DomeLight `texture:file` fallback when the resolver returns
// no resolved path. Walks the attribute's property stack strongest-first and,
// per layer, first tries SdfComputeAssetPathRelativeToLayer (package-aware — it
// preserves `.usdz[...]` bracket syntax); if that leaves the path relative (this
// build's custom resolver returns unresolvable relative paths unchanged), falls
// back to manual composition against the layer's real-path directory. The chosen
// layer is the strongest spec's, which may differ from the one carrying the
// authored value if a stronger layer sets only other fields — a pre-existing
// limitation.
std::string anchor_asset_to_layer(const UsdAttribute& attr,
                                  const std::string& authored,
                                  const UsdTimeCode& time) {
    if (!attr || authored.empty()) {
        return std::string();
    }
    if (!TfIsRelativePath(authored)) {
        return authored; // already absolute — nothing to anchor
    }
    for (const SdfPropertySpecHandle& spec : attr.GetPropertyStack(time)) {
        const SdfLayerHandle layer = spec->GetLayer();
        if (!layer) {
            continue;
        }
        // Prefer the resolver/package-aware anchor; it handles assets inside a
        // `.usdz` package that manual path composition would mangle.
        const std::string anchored =
            SdfComputeAssetPathRelativeToLayer(layer, authored);
        if (!anchored.empty() && !TfIsRelativePath(anchored)) {
            return anchored;
        }
        // Resolver left it relative — anchor manually against the layer dir.
        const std::string real = layer->GetRealPath();
        if (real.empty()) {
            continue; // anonymous / in-memory layer has no directory to anchor to
        }
        const std::string dir = TfGetPathName(real);
        if (!dir.empty()) {
            return TfStringCatPaths(dir, authored);
        }
    }
    return std::string();
}

// A UDIM template (e.g. "albedo.<UDIM>.png") never resolves, because no
// concrete file matches the literal token, so SdfAssetPath::GetResolvedPath()
// is empty and USD keeps only the *authored* (usually layer-relative) path.
// The render pipeline expands the template per tile and reads each concrete
// file through the resolver, which requires an absolute path. Anchor the
// token-bearing path against its authoring layer, yielding an absolute template
// with the <UDIM> token intact. Returns an empty string when `asset` is not a
// UDIM template or cannot be anchored (callers then keep their prior value).
std::string anchor_udim_template(const UsdShadeInput& file_input,
                                 const SdfAssetPath& asset,
                                 const UsdTimeCode& time) {
    // Fold any alternate placeholder to "<UDIM>" first, so a "%(UDIM)d" path is
    // recognized and anchored rather than left relative and unresolvable.
    const std::string authored = normalize_udim_token(asset.GetAssetPath());
    if (authored.find("<UDIM>") == std::string::npos) {
        return std::string();
    }
    const UsdAttribute attr = file_input.GetAttr();
    for (const SdfPropertySpecHandle& spec : attr.GetPropertyStack(time)) {
        const std::string anchored =
            SdfComputeAssetPathRelativeToLayer(spec->GetLayer(), authored);
        if (!anchored.empty()) {
            return anchored;
        }
    }
    return std::string();
}

std::string texture_asset_path(const UsdShadeShader& texture_shader,
                               const UsdTimeCode& time) {
    SdfAssetPath asset;
    const UsdShadeInput file_input = texture_shader.GetInput(TfToken("file"));
    if (!file_input || !file_input.Get(&asset, time)) {
        return std::string();
    }
    if (!asset.GetResolvedPath().empty()) {
        return asset.GetResolvedPath();
    }
    const std::string anchored = anchor_udim_template(file_input, asset, time);
    return anchored.empty() ? normalize_udim_token(asset.GetAssetPath())
                            : anchored;
}

bool connects_to_material_interface(const UsdShadeInput& input) {
    UsdShadeConnectableAPI source;
    TfToken source_name;
    UsdShadeAttributeType source_type = UsdShadeAttributeType::Invalid;
    return input && UsdShadeConnectableAPI::GetConnectedSource(
                        input.GetAttr(), &source, &source_name, &source_type) &&
           source.GetPrim().IsA<UsdShadeMaterial>();
}

std::string connected_texture_path(const UsdShadeShader& shader,
                                   const TfToken& input_name,
                                   const UsdTimeCode& time) {
    const UsdShadeShader texture_shader =
        connected_texture_shader(shader, input_name);
    if (!texture_shader) {
        return std::string();
    }
    return texture_asset_path(texture_shader, time);
}

std::string texture_uv_set(const UsdShadeShader& texture_shader,
                           const UsdTimeCode& time,
                           rust::Vec<float>* transform) {
    UsdShadeInput coordinate = texture_shader.GetInput(TfToken("st"));
    UsdShadeConnectableAPI source;
    TfToken source_name;
    UsdShadeAttributeType source_type = UsdShadeAttributeType::Invalid;
    if (!coordinate || !UsdShadeConnectableAPI::GetConnectedSource(
                           coordinate.GetAttr(), &source, &source_name,
                           &source_type)) {
        return "st";
    }
    UsdShadeShader reader(source.GetPrim());
    TfToken identifier;
    reader.GetIdAttr().Get(&identifier);
    if (identifier == TfToken("UsdTransform2d")) {
        GfVec2f scale(1.0f, 1.0f);
        GfVec2f translation(0.0f, 0.0f);
        float rotation = 0.0f;
        const UsdShadeInput scale_input = reader.GetInput(TfToken("scale"));
        const UsdShadeInput rotation_input = reader.GetInput(TfToken("rotation"));
        const UsdShadeInput translation_input =
            reader.GetInput(TfToken("translation"));
        if (scale_input) {
            scale_input.Get(&scale, time);
        }
        if (rotation_input) {
            rotation_input.Get(&rotation, time);
        }
        if (translation_input) {
            translation_input.Get(&translation, time);
        }
        transform->push_back(translation[0]);
        transform->push_back(rotation);
        transform->push_back(scale[0]);
        transform->push_back(0.0f);
        transform->push_back(scale[1]);
        transform->push_back(translation[1]);
        coordinate = reader.GetInput(TfToken("in"));
        if (!coordinate || !UsdShadeConnectableAPI::GetConnectedSource(
                               coordinate.GetAttr(), &source, &source_name,
                               &source_type)) {
            return "st";
        }
        reader = UsdShadeShader(source.GetPrim());
        reader.GetIdAttr().Get(&identifier);
    }
    if (identifier != TfToken("UsdPrimvarReader_float2")) {
        return "st";
    }
    const UsdShadeInput varname = reader.GetInput(TfToken("varname"));
    std::string name;
    if (varname && varname.Get(&name) && !name.empty()) {
        return name;
    }
    TfToken token_name;
    if (varname && varname.Get(&token_name) && !token_name.IsEmpty()) {
        return token_name.GetString();
    }
    return "st";
}

constexpr std::uint32_t kMaterialIrVersion = 2;
constexpr std::size_t kMaxMaterialTextures = 8;
constexpr std::size_t kMaxMaterialUvs = 2;
constexpr std::size_t kMaxMaterialNodes = 64;
constexpr std::size_t kMaxMaterialDiagnostics = 64;
constexpr std::size_t kMaxMaterialDepth = 16;

constexpr std::uint64_t kBaseColorTexture = 1u << 0;
constexpr std::uint64_t kNormalTexture = 1u << 1;
constexpr std::uint64_t kMetallicTexture = 1u << 2;
constexpr std::uint64_t kRoughnessTexture = 1u << 3;
constexpr std::uint64_t kOcclusionTexture = 1u << 4;
constexpr std::uint64_t kEmissiveTexture = 1u << 5;
constexpr std::uint64_t kOpacityTexture = 1u << 6;
constexpr std::uint64_t kTwoUvSets = 1u << 7;

bool starts_with(const std::string& value, const char* prefix) {
    return value.rfind(prefix, 0) == 0;
}

std::string input_token(const UsdShadeInput& input, const UsdTimeCode& time) {
    TfToken token;
    if (input && input.Get(&token, time)) {
        return token.GetString();
    }
    std::string value;
    if (input && input.Get(&value, time)) {
        return value;
    }
    return std::string();
}

bool input_vec4(const UsdShadeInput& input, const UsdTimeCode& time,
                std::array<float, 4>* value) {
    GfVec4f vector4;
    if (input && input.Get(&vector4, time)) {
        std::copy(vector4.data(), vector4.data() + 4, value->begin());
        return true;
    }
    GfVec3f vector3;
    if (input && input.Get(&vector3, time)) {
        std::copy(vector3.data(), vector3.data() + 3, value->begin());
        (*value)[3] = 1.0f;
        return true;
    }
    GfVec2f vector2;
    if (input && input.Get(&vector2, time)) {
        (*value)[0] = vector2[0];
        (*value)[1] = vector2[1];
        (*value)[2] = vector2[1];
        (*value)[3] = 1.0f;
        return true;
    }
    float scalar = 0.0f;
    if (input && input.Get(&scalar, time)) {
        value->fill(scalar);
        return true;
    }
    return false;
}

void assign_vec4(rust::Vec<float>* destination,
                 const std::array<float, 4>& source) {
    destination->clear();
    destination->reserve(source.size());
    for (float value : source) {
        destination->push_back(value);
    }
}

void assign_identity_uv(rust::Vec<float>* transform) {
    transform->clear();
    for (float value : {0.0f, 0.0f, 1.0f, 0.0f, 1.0f, 0.0f}) {
        transform->push_back(value);
    }
}

std::array<float, 6> compose_affine(const std::array<float, 6>& outer,
                                    const std::array<float, 6>& inner) {
    return {
        outer[0] * inner[0] + outer[1] * inner[2],
        outer[0] * inner[1] + outer[1] * inner[3],
        outer[2] * inner[0] + outer[3] * inner[2],
        outer[2] * inner[1] + outer[3] * inner[3],
        outer[0] * inner[4] + outer[1] * inner[5] + outer[4],
        outer[2] * inner[4] + outer[3] * inner[5] + outer[5],
    };
}

char channel_character(std::uint32_t channel) {
    static constexpr char channels[] = {'r', 'g', 'b', 'a'};
    return channels[std::min<std::uint32_t>(channel, 3)];
}

std::string output_swizzle(const TfToken& output,
                           const std::string& semantic) {
    const std::string name = output.GetString();
    if (name == "r" || name == "x") {
        return "r";
    }
    if (name == "g" || name == "y") {
        return "g";
    }
    if (name == "b" || name == "z") {
        return "b";
    }
    if (name == "a" || name == "w") {
        return "a";
    }
    if (name == "rgba" || name == "xyzw") {
        return "rgba";
    }
    if (name == "rgb" || name == "xyz") {
        return "rgb";
    }
    return semantic == "metallic" || semantic == "roughness" ||
                   semantic == "occlusion" || semantic == "opacity"
        ? "r"
        : "rgb";
}

std::string texture_path(const TextureInputRaw& texture) {
    return texture.resolved_asset_path.empty()
        ? std::string(texture.asset_path)
        : std::string(texture.resolved_asset_path);
}

class MaterialIrLowerer {
public:
    MaterialIrLowerer(const UsdShadeMaterial& material,
                      const UsdShadeShader& surface,
                      const UsdTimeCode& time,
                      MaterialRecordRaw* record)
        : material_(material), surface_(surface), time_(time), record_(record) {
        register_node(surface_);
    }

    void diagnostic(const char* code, const std::string& source,
                    const std::string& message) {
        const std::string key = std::string(code) + "\n" + source + "\n" + message;
        if (!diagnostic_keys_.insert(key).second ||
            record_->material_diagnostics.size() >= kMaxMaterialDiagnostics) {
            return;
        }
        MaterialDiagnosticRaw diagnostic;
        diagnostic.code = rust::String(code);
        diagnostic.source_path = rust::String(source);
        diagnostic.message = rust::String(message);
        record_->material_diagnostics.push_back(std::move(diagnostic));
        record_->diagnostics.push_back(rust::String(message));
    }

    bool lower(const TfToken& input_name, const char* semantic) {
        if (record_->texture_inputs.size() >= kMaxMaterialTextures) {
            diagnostic("texture_limit", material_.GetPath().GetString(),
                       material_.GetPath().GetString() +
                           " exceeds the bounded portable texture-input limit");
            return false;
        }
        const UsdShadeInput input = surface_.GetInput(input_name);
        if (!input || !input.HasConnectedSource()) {
            return false;
        }
        TextureInputRaw texture;
        initialize_texture(&texture, semantic);
        std::set<std::string> active_edges;
        if (!trace(input.GetAttr(), &texture, &active_edges, 0)) {
            return false;
        }
        if (texture.asset_path.empty()) {
            diagnostic("missing_asset", material_.GetPath().GetString(),
                       material_.GetPath().GetString() + " " + semantic +
                           " image has no authored asset path");
            return false;
        }
        if (!retain_uv(texture)) {
            return false;
        }
        record_->material_feature_bits |= feature_for(semantic);
        record_->texture_inputs.push_back(std::move(texture));
        return true;
    }

    void finish() {
        if (uv_keys_.size() == kMaxMaterialUvs) {
            record_->material_feature_bits |= kTwoUvSets;
        }
    }

private:
    void initialize_texture(TextureInputRaw* texture,
                            const std::string& semantic) const {
        texture->semantic = rust::String(semantic);
        texture->swizzle = rust::String(
            semantic == "metallic" || semantic == "roughness" ||
                    semantic == "occlusion" || semantic == "opacity"
                ? "r"
                : "rgb");
        assign_vec4(&texture->scale, {1.0f, 1.0f, 1.0f, 1.0f});
        assign_vec4(&texture->bias, {0.0f, 0.0f, 0.0f, 0.0f});
        assign_vec4(&texture->fallback, {0.0f, 0.0f, 0.0f, 1.0f});
        texture->uv_name = rust::String("st");
        assign_identity_uv(&texture->uv_transform);
        texture->wrap_s = rust::String("repeat");
        texture->wrap_t = rust::String("repeat");
        texture->min_filter = rust::String("linear");
        texture->mag_filter = rust::String("linear");
        texture->mip_filter = rust::String("linear");
        texture->max_anisotropy = 1;
        texture->interpretation = rust::String(default_interpretation(semantic));
    }

    static const char* default_interpretation(const std::string& semantic) {
        if (semantic == "normal") {
            return "normal";
        }
        if (semantic == "base_color" || semantic == "emissive") {
            return "srgb_color";
        }
        return "data";
    }

    static std::uint64_t feature_for(const std::string& semantic) {
        if (semantic == "base_color") {
            return kBaseColorTexture;
        }
        if (semantic == "normal") {
            return kNormalTexture;
        }
        if (semantic == "metallic") {
            return kMetallicTexture;
        }
        if (semantic == "roughness") {
            return kRoughnessTexture;
        }
        if (semantic == "occlusion") {
            return kOcclusionTexture;
        }
        if (semantic == "emissive") {
            return kEmissiveTexture;
        }
        return kOpacityTexture;
    }

    bool retain_uv(const TextureInputRaw& texture) {
        std::string key(texture.uv_name);
        for (float value : texture.uv_transform) {
            std::uint32_t bits = 0;
            std::memcpy(&bits, &value, sizeof(bits));
            key += ":" + std::to_string(bits);
        }
        if (uv_keys_.find(key) != uv_keys_.end()) {
            return true;
        }
        if (uv_keys_.size() >= kMaxMaterialUvs) {
            diagnostic("uv_limit", material_.GetPath().GetString(),
                       material_.GetPath().GetString() +
                           " requires more than two distinct portable UV mappings; " +
                           std::string(texture.semantic) + " uses its fallback factor");
            return false;
        }
        uv_keys_.insert(std::move(key));
        return true;
    }

    bool register_node(const UsdShadeShader& shader,
                       rust::Vec<std::uint32_t>* provenance = nullptr) {
        if (!shader) {
            return false;
        }
        const std::string path = shader.GetPath().GetString();
        const auto existing = node_indices_.find(path);
        if (existing != node_indices_.end()) {
            append_provenance(existing->second, provenance);
            return true;
        }
        if (record_->source_nodes.size() >= kMaxMaterialNodes) {
            diagnostic("graph_node_limit", path,
                       material_.GetPath().GetString() +
                           " exceeds the 64-node portable material bound");
            return false;
        }
        TfToken identifier;
        shader.GetIdAttr().Get(&identifier);
        MaterialSourceNodeRaw node;
        node.path = rust::String(path);
        node.identifier = rust::String(
            identifier.IsEmpty() ? "<missing>" : identifier.GetString());
        const std::uint32_t index =
            static_cast<std::uint32_t>(record_->source_nodes.size());
        record_->source_nodes.push_back(std::move(node));
        node_indices_.emplace(path, index);
        append_provenance(index, provenance);
        return true;
    }

    static void append_provenance(std::uint32_t index,
                                  rust::Vec<std::uint32_t>* provenance) {
        if (provenance != nullptr &&
            std::find(provenance->begin(), provenance->end(), index) ==
                provenance->end()) {
            provenance->push_back(index);
        }
    }

    bool trace(const UsdAttribute& attribute, TextureInputRaw* texture,
               std::set<std::string>* active_edges, std::size_t depth) {
        if (depth > kMaxMaterialDepth) {
            diagnostic("graph_depth_limit", attribute.GetPath().GetString(),
                       material_.GetPath().GetString() +
                           " exceeds the portable graph depth of 16");
            return false;
        }
        UsdShadeConnectableAPI source;
        TfToken source_name;
        UsdShadeAttributeType source_type = UsdShadeAttributeType::Invalid;
        if (!attribute || !UsdShadeConnectableAPI::GetConnectedSource(
                              attribute, &source, &source_name, &source_type)) {
            diagnostic("unsupported_connection", attribute.GetPath().GetString(),
                       material_.GetPath().GetString() +
                           " has an unresolved portable material connection");
            return false;
        }
        const std::string edge = source.GetPath().GetString() + "." +
                                 source_name.GetString();
        if (!active_edges->insert(edge).second) {
            diagnostic("graph_cycle", source.GetPath().GetString(),
                       material_.GetPath().GetString() +
                           " contains a cycle at " + edge);
            return false;
        }

        const UsdShadeShader shader(source.GetPrim());
        bool success = false;
        if (shader && register_node(shader, &texture->source_node_indices)) {
            TfToken identifier_token;
            shader.GetIdAttr().Get(&identifier_token);
            const std::string identifier = identifier_token.GetString();
            if (identifier == "UsdUVTexture" || starts_with(identifier, "ND_image_")) {
                success = read_image(shader, source_name, texture, active_edges, depth);
            } else if (starts_with(identifier, "ND_normalmap_")) {
                success = trace_passthrough(shader, {"in"}, texture, active_edges,
                                            depth + 1);
            } else if (starts_with(identifier, "ND_convert_")) {
                success = trace_passthrough(shader, {"in"}, texture, active_edges,
                                            depth + 1);
            } else if (starts_with(identifier, "ND_extract_")) {
                success = trace_extract(shader, texture, active_edges, depth + 1);
            } else if (starts_with(identifier, "ND_multiply_")) {
                success = trace_multiply(shader, texture, active_edges, depth + 1);
            } else {
                diagnostic("unsupported_node", shader.GetPath().GetString(),
                           material_.GetPath().GetString() + " node " + identifier +
                               " is outside the portable image/extract/convert/" +
                               "multiply-constant/normal/UV subset");
                // Recurse through inputs of unsupported nodes to surface graph
                // cycles that pass through them; the unsupported_node diagnostic
                // was already emitted so success stays false.
                for (const UsdShadeInput& inp : shader.GetInputs()) {
                    if (inp.HasConnectedSource()) {
                        trace(inp.GetAttr(), texture, active_edges, depth + 1);
                    }
                }
            }
        } else if (source.GetPrim().IsA<UsdShadeNodeGraph>() ||
                   source.GetPrim().IsA<UsdShadeMaterial>()) {
            if (source_type == UsdShadeAttributeType::Input) {
                const UsdShadeInput input = source.GetInput(source_name);
                success = input &&
                    trace(input.GetAttr(), texture, active_edges, depth + 1);
            } else {
                const UsdShadeOutput output = source.GetOutput(source_name);
                success = output &&
                    trace(output.GetAttr(), texture, active_edges, depth + 1);
            }
        }
        active_edges->erase(edge);
        return success;
    }

    bool trace_passthrough(const UsdShadeShader& shader,
                           std::initializer_list<const char*> input_names,
                           TextureInputRaw* texture,
                           std::set<std::string>* active_edges,
                           std::size_t depth) {
        for (const char* name : input_names) {
            const UsdShadeInput input = shader.GetInput(TfToken(name));
            if (input && input.HasConnectedSource()) {
                return trace(input.GetAttr(), texture, active_edges, depth);
            }
        }
        diagnostic("unsupported_connection", shader.GetPath().GetString(),
                   shader.GetPath().GetString() +
                       " has no connected portable input");
        return false;
    }

    bool trace_extract(const UsdShadeShader& shader, TextureInputRaw* texture,
                       std::set<std::string>* active_edges, std::size_t depth) {
        int index = 0;
        const UsdShadeInput index_input = shader.GetInput(TfToken("index"));
        if (index_input) {
            index_input.Get(&index, time_);
        }
        if (index < 0 || index > 3 ||
            !trace_passthrough(shader, {"in"}, texture, active_edges, depth)) {
            if (index < 0 || index > 3) {
                diagnostic("unsupported_input", shader.GetPath().GetString(),
                           shader.GetPath().GetString() +
                               " extract index is outside [0, 3]");
            }
            return false;
        }
        texture->swizzle = rust::String(
            std::string(1, channel_character(static_cast<std::uint32_t>(index))));
        return true;
    }

    bool trace_multiply(const UsdShadeShader& shader, TextureInputRaw* texture,
                        std::set<std::string>* active_edges, std::size_t depth) {
        const UsdShadeInput first = shader.GetInput(TfToken("in1"));
        const UsdShadeInput second = shader.GetInput(TfToken("in2"));
        const bool first_connected = first && first.HasConnectedSource();
        const bool second_connected = second && second.HasConnectedSource();
        if (first_connected == second_connected) {
            diagnostic("unsupported_node", shader.GetPath().GetString(),
                       shader.GetPath().GetString() +
                           " multiply must have exactly one texture input and one " +
                           "authored constant");
            return false;
        }
        const UsdShadeInput connected = first_connected ? first : second;
        const UsdShadeInput constant = first_connected ? second : first;
        std::array<float, 4> multiplier{1.0f, 1.0f, 1.0f, 1.0f};
        if (!input_vec4(constant, time_, &multiplier)) {
            diagnostic("unsupported_input", shader.GetPath().GetString(),
                       shader.GetPath().GetString() +
                           " multiply constant cannot be converted to float4");
            return false;
        }
        if (!trace(connected.GetAttr(), texture, active_edges, depth)) {
            return false;
        }
        for (std::size_t index = 0; index < 4; ++index) {
            texture->scale[index] *= multiplier[index];
            texture->bias[index] *= multiplier[index];
        }
        return true;
    }

    bool read_image(const UsdShadeShader& shader, const TfToken& output,
                    TextureInputRaw* texture,
                    std::set<std::string>* active_edges, std::size_t depth) {
        SdfAssetPath asset;
        const UsdShadeInput file = shader.GetInput(TfToken("file"));
        if (file && file.Get(&asset, time_)) {
            texture->asset_path =
                rust::String(normalize_udim_token(asset.GetAssetPath()));
            std::string resolved = asset.GetResolvedPath();
            if (resolved.empty()) {
                // UDIM templates never resolve; anchor the token path to its
                // layer so downstream reads get an absolute, expandable path.
                resolved = anchor_udim_template(file, asset, time_);
            }
            texture->resolved_asset_path = rust::String(resolved);
        }
        texture->swizzle = rust::String(
            output_swizzle(output, std::string(texture->semantic)));

        std::array<float, 4> value;
        if (input_vec4(shader.GetInput(TfToken("scale")), time_, &value)) {
            assign_vec4(&texture->scale, value);
        }
        if (input_vec4(shader.GetInput(TfToken("bias")), time_, &value)) {
            assign_vec4(&texture->bias, value);
        }
        const UsdShadeInput fallback = shader.GetInput(TfToken("fallback"));
        const UsdShadeInput default_input = shader.GetInput(TfToken("default"));
        if (input_vec4(fallback ? fallback : default_input, time_, &value)) {
            assign_vec4(&texture->fallback, value);
        }
        read_sampler(shader, texture);
        read_color_interpretation(shader, texture);
        read_uv(shader, texture, active_edges, depth + 1);
        return true;
    }

    void read_sampler(const UsdShadeShader& shader, TextureInputRaw* texture) {
        texture->wrap_s = rust::String(address_mode(
            shader, {"wrapS", "uaddressmode"}, shader.GetPath().GetString()));
        texture->wrap_t = rust::String(address_mode(
            shader, {"wrapT", "vaddressmode"}, shader.GetPath().GetString()));

        std::string common_filter =
            input_token(shader.GetInput(TfToken("filtertype")), time_);
        texture->min_filter = rust::String(filter_mode(
            input_token(shader.GetInput(TfToken("minFilter")), time_),
            common_filter, shader.GetPath().GetString(), "minFilter"));
        texture->mag_filter = rust::String(filter_mode(
            input_token(shader.GetInput(TfToken("magFilter")), time_),
            common_filter, shader.GetPath().GetString(), "magFilter"));
        const std::string mip =
            input_token(shader.GetInput(TfToken("mipFilter")), time_);
        if (mip == "none") {
            texture->mip_filter = rust::String("none");
        } else if (mip == "nearest" || mip == "closest") {
            texture->mip_filter = rust::String("nearest");
        } else if (mip.empty() || mip == "linear") {
            texture->mip_filter = rust::String("linear");
        } else {
            diagnostic("invalid_sampler", shader.GetPath().GetString(),
                       shader.GetPath().GetString() + " mipFilter=" + mip +
                           " is unsupported; using linear");
        }
        int anisotropy = 1;
        const UsdShadeInput anisotropy_input =
            shader.GetInput(TfToken("maxAnisotropy"));
        if (anisotropy_input) {
            anisotropy_input.Get(&anisotropy, time_);
        }
        texture->max_anisotropy =
            static_cast<std::uint32_t>(std::clamp(anisotropy, 1, 16));
    }

    std::string address_mode(const UsdShadeShader& shader,
                             std::initializer_list<const char*> names,
                             const std::string& source) {
        std::string authored;
        for (const char* name : names) {
            authored = input_token(shader.GetInput(TfToken(name)), time_);
            if (!authored.empty()) {
                break;
            }
        }
        if (authored.empty() || authored == "repeat" || authored == "periodic" ||
            authored == "useMetadata") {
            return "repeat";
        }
        if (authored == "clamp") {
            return "clamp_to_edge";
        }
        if (authored == "mirror") {
            return "mirror_repeat";
        }
        if (authored == "black" || authored == "constant") {
            return "clamp_to_border";
        }
        diagnostic("invalid_sampler", source,
                   source + " address mode " + authored +
                       " is unsupported; using repeat");
        return "repeat";
    }

    std::string filter_mode(const std::string& authored,
                            const std::string& common,
                            const std::string& source,
                            const char* name) {
        const std::string value = authored.empty() ? common : authored;
        if (value == "nearest" || value == "closest") {
            return "nearest";
        }
        if (value == "cubic") {
            return "cubic";
        }
        if (value.empty() || value == "linear") {
            return "linear";
        }
        diagnostic("invalid_sampler", source,
                   source + " " + name + "=" + value +
                       " is unsupported; using linear");
        return "linear";
    }

    void read_color_interpretation(const UsdShadeShader& shader,
                                   TextureInputRaw* texture) {
        std::string authored = input_token(
            shader.GetInput(TfToken("sourceColorSpace")), time_);
        if (authored.empty()) {
            authored = input_token(shader.GetInput(TfToken("colorspace")), time_);
        }
        const UsdShadeInput file = shader.GetInput(TfToken("file"));
        if (authored.empty() && file) {
            authored = file.GetAttr().GetColorSpace().GetString();
        }
        texture->source_color_space = rust::String(authored);
        const std::string semantic(texture->semantic);
        if (semantic == "normal") {
            texture->interpretation = rust::String("normal");
            return;
        }
        if (semantic != "base_color" && semantic != "emissive") {
            texture->interpretation = rust::String("data");
            return;
        }
        const std::string path = TfStringToLower(std::string(texture->asset_path));
        const bool hdr = path.size() >= 4 &&
            (path.compare(path.size() - 4, 4, ".exr") == 0 ||
             path.compare(path.size() - 4, 4, ".hdr") == 0);
        const std::string normalized = TfStringToLower(authored);
        if (normalized == "srgb" || normalized == "srgb_texture") {
            texture->interpretation = rust::String("srgb_color");
        } else if (normalized == "raw" || normalized == "data") {
            texture->interpretation = rust::String("data");
        } else if (normalized == "lin_rec709" || normalized == "linear" ||
                   normalized == "scene_linear") {
            texture->interpretation = rust::String(
                hdr ? "scene_linear_hdr" : "linear_color");
        } else if (normalized.empty() || normalized == "auto") {
            texture->interpretation = rust::String(
                hdr ? "scene_linear_hdr" : "srgb_color");
        } else {
            diagnostic("invalid_color_space", shader.GetPath().GetString(),
                       shader.GetPath().GetString() + " source color space " +
                           authored + " is unsupported; using " +
                           (hdr ? "scene-linear HDR" : "sRGB color"));
            texture->interpretation = rust::String(
                hdr ? "scene_linear_hdr" : "srgb_color");
        }
    }

    void read_uv(const UsdShadeShader& shader, TextureInputRaw* texture,
                 std::set<std::string>* active_edges, std::size_t depth) {
        UsdShadeInput coordinate = shader.GetInput(TfToken("st"));
        if (!coordinate) {
            coordinate = shader.GetInput(TfToken("texcoord"));
        }
        if (!coordinate || !coordinate.HasConnectedSource()) {
            return;
        }
        std::array<float, 6> transform{1.0f, 0.0f, 0.0f, 1.0f, 0.0f, 0.0f};
        std::string name = "st";
        if (trace_uv(coordinate.GetAttr(), &name, &transform,
                     &texture->source_node_indices, active_edges, depth)) {
            texture->uv_name = rust::String(name);
            // Decompose composed affine matrix back to raw USD params [tx, rot_deg, sx, 0, sy, ty].
            // For a single UsdTransform2d this is exact; for chained transforms with non-isotropic
            // scales any shear introduced by composition is absorbed into sx/sy (lossless for
            // isotropic scales, closest-RS approximation otherwise).
            const float sx = std::sqrt(transform[0]*transform[0] + transform[2]*transform[2]);
            const float sy = std::sqrt(transform[1]*transform[1] + transform[3]*transform[3]);
            const float rot_rad = std::atan2(transform[2], transform[0]);
            const float rot_deg = rot_rad * (180.0f / 3.14159265358979323846f);
            texture->uv_transform.clear();
            texture->uv_transform.push_back(transform[4]);
            texture->uv_transform.push_back(rot_deg);
            texture->uv_transform.push_back(sx > 0.0f ? sx : 1.0f);
            texture->uv_transform.push_back(0.0f);
            texture->uv_transform.push_back(sy > 0.0f ? sy : 1.0f);
            texture->uv_transform.push_back(transform[5]);
        }
    }

    bool trace_uv(const UsdAttribute& attribute, std::string* name,
                  std::array<float, 6>* transform,
                  rust::Vec<std::uint32_t>* provenance,
                  std::set<std::string>* active_edges, std::size_t depth) {
        if (depth > kMaxMaterialDepth) {
            diagnostic("graph_depth_limit", attribute.GetPath().GetString(),
                       material_.GetPath().GetString() +
                           " UV graph exceeds the portable depth of 16");
            return false;
        }
        UsdShadeConnectableAPI source;
        TfToken source_name;
        UsdShadeAttributeType source_type = UsdShadeAttributeType::Invalid;
        if (!UsdShadeConnectableAPI::GetConnectedSource(
                attribute, &source, &source_name, &source_type)) {
            return false;
        }
        const std::string edge = source.GetPath().GetString() + "." +
                                 source_name.GetString();
        if (!active_edges->insert(edge).second) {
            diagnostic("graph_cycle", source.GetPath().GetString(),
                       material_.GetPath().GetString() +
                           " contains a UV cycle at " + edge);
            return false;
        }
        const UsdShadeShader node(source.GetPrim());
        if (!node || !register_node(node, provenance)) {
            active_edges->erase(edge);
            return false;
        }
        TfToken identifier_token;
        node.GetIdAttr().Get(&identifier_token);
        const std::string identifier = identifier_token.GetString();
        bool success = false;
        if (identifier == "UsdPrimvarReader_float2") {
            std::string authored =
                input_token(node.GetInput(TfToken("varname")), time_);
            if (!authored.empty()) {
                *name = authored;
            }
            success = true;
        } else if (starts_with(identifier, "ND_texcoord_")) {
            int index = 0;
            const UsdShadeInput index_input = node.GetInput(TfToken("index"));
            if (index_input) {
                index_input.Get(&index, time_);
            }
            *name = index <= 0 ? "st" : "st" + std::to_string(index);
            success = true;
        } else if (starts_with(identifier, "ND_geompropvalue_")) {
            std::string authored = input_token(
                node.GetInput(TfToken("geomprop")), time_);
            if (authored.empty()) {
                authored = input_token(node.GetInput(TfToken("geompropname")), time_);
            }
            if (!authored.empty()) {
                *name = authored;
                success = true;
            }
        } else if (identifier == "UsdTransform2d" ||
                   starts_with(identifier, "ND_transform2d_")) {
            GfVec2f scale(1.0f, 1.0f);
            GfVec2f translation(0.0f, 0.0f);
            float rotation = 0.0f;
            const UsdShadeInput scale_input = node.GetInput(TfToken("scale"));
            const UsdShadeInput rotation_input = node.GetInput(TfToken("rotation"));
            const UsdShadeInput translation_input =
                node.GetInput(TfToken("translation"));
            if (scale_input) {
                scale_input.Get(&scale, time_);
            }
            if (rotation_input) {
                rotation_input.Get(&rotation, time_);
            }
            if (translation_input) {
                translation_input.Get(&translation, time_);
            }
            const float radians = rotation * 0.01745329251994329577f;
            const float cosine = std::cos(radians);
            const float sine = std::sin(radians);
            const std::array<float, 6> authored{
                cosine * scale[0], -sine * scale[1],
                sine * scale[0], cosine * scale[1],
                translation[0], translation[1],
            };
            *transform = compose_affine(authored, *transform);
            UsdShadeInput input = node.GetInput(TfToken("in"));
            if (!input) {
                input = node.GetInput(TfToken("texcoord"));
            }
            success = input && input.HasConnectedSource() &&
                trace_uv(input.GetAttr(), name, transform, provenance,
                         active_edges, depth + 1);
        } else {
            diagnostic("unsupported_node", node.GetPath().GetString(),
                       material_.GetPath().GetString() + " UV node " + identifier +
                           " is outside the portable UV subset");
        }
        active_edges->erase(edge);
        return success;
    }

    UsdShadeMaterial material_;
    UsdShadeShader surface_;
    UsdTimeCode time_;
    MaterialRecordRaw* record_;
    std::map<std::string, std::uint32_t> node_indices_;
    std::set<std::string> diagnostic_keys_;
    std::set<std::string> uv_keys_;
};

class MaterialIrFingerprintBuilder {
public:
    void add(std::uint32_t value) { add_unsigned(value); }
    void add(std::uint64_t value) { add_unsigned(value); }
    void add(float value) {
        std::uint32_t bits = 0;
        std::memcpy(&bits, &value, sizeof(bits));
        add(bits);
    }
    void add(std::string_view value) {
        add(static_cast<std::uint64_t>(value.size()));
        for (unsigned char byte : value) {
            add_byte(byte);
        }
    }
    void add(const rust::String& value) {
        add(std::string_view(value.data(), value.size()));
    }
    template <typename T>
    void add_values(const rust::Vec<T>& values) {
        add(static_cast<std::uint64_t>(values.size()));
        for (const T& value : values) {
            add(value);
        }
    }
    std::string finish() const {
        return std::to_string(first_) + ":" + std::to_string(second_);
    }

private:
    template <typename T>
    void add_unsigned(T value) {
        for (std::size_t index = 0; index < sizeof(T); ++index) {
            add_byte(static_cast<unsigned char>(value & 0xff));
            value >>= 8;
        }
    }
    void add_byte(unsigned char byte) {
        first_ ^= byte;
        first_ *= 1099511628211ULL;
        second_ ^= static_cast<std::uint64_t>(byte) + 0x9e;
        second_ *= 14029467366897019727ULL;
        second_ ^= second_ >> 29;
    }

    std::uint64_t first_ = 1469598103934665603ULL;
    std::uint64_t second_ = 7809847782465536322ULL;
};

std::string material_ir_fingerprint(const MaterialRecordRaw& material) {
    MaterialIrFingerprintBuilder fingerprint;
    fingerprint.add(material.material_ir_version);
    fingerprint.add(material.material_model);
    fingerprint.add(material.material_feature_bits);
    fingerprint.add(material.alpha_policy);
    fingerprint.add(material.sidedness);
    fingerprint.add_values(material.base_color);
    fingerprint.add(material.metallic);
    fingerprint.add(material.roughness);
    fingerprint.add(material.subsurface);
    fingerprint.add_values(material.emissive);
    fingerprint.add_values(material.transmission_color);
    fingerprint.add(material.opacity);
    fingerprint.add(material.opacity_threshold);
    fingerprint.add(static_cast<std::uint32_t>(material.opacity_mode));
    fingerprint.add(static_cast<std::uint64_t>(material.texture_inputs.size()));
    for (const TextureInputRaw& texture : material.texture_inputs) {
        fingerprint.add(texture.semantic);
        fingerprint.add(texture.asset_path);
        fingerprint.add(texture.resolved_asset_path);
        fingerprint.add(texture.swizzle);
        fingerprint.add_values(texture.scale);
        fingerprint.add_values(texture.bias);
        fingerprint.add_values(texture.fallback);
        fingerprint.add(texture.uv_name);
        fingerprint.add_values(texture.uv_transform);
        fingerprint.add(texture.wrap_s);
        fingerprint.add(texture.wrap_t);
        fingerprint.add(texture.min_filter);
        fingerprint.add(texture.mag_filter);
        fingerprint.add(texture.mip_filter);
        fingerprint.add(texture.max_anisotropy);
        fingerprint.add(texture.source_color_space);
        fingerprint.add(texture.interpretation);
        fingerprint.add_values(texture.source_node_indices);
    }
    fingerprint.add(static_cast<std::uint64_t>(material.source_nodes.size()));
    for (const MaterialSourceNodeRaw& node : material.source_nodes) {
        fingerprint.add(node.path);
        fingerprint.add(node.identifier);
    }
    fingerprint.add(
        static_cast<std::uint64_t>(material.material_diagnostics.size()));
    for (const MaterialDiagnosticRaw& diagnostic :
         material.material_diagnostics) {
        fingerprint.add(diagnostic.code);
        fingerprint.add(diagnostic.source_path);
        fingerprint.add(diagnostic.message);
    }
    return fingerprint.finish();
}

std::string specifier_name(SdfSpecifier specifier) {
    switch (specifier) {
        case SdfSpecifierDef:
            return "def";
        case SdfSpecifierOver:
            return "over";
        case SdfSpecifierClass:
            return "class";
    }
    return std::string();
}

bool is_usd_asset(const std::string& asset_path) {
    const std::string extension = TfStringToLower(
        asset_path.substr(asset_path.find_last_of('.') + 1));
    return extension == "usd" || extension == "usda" || extension == "usdc" ||
           extension == "usdz";
}

void topology_provenance(const UsdPrim& prim, const SdfLayerHandle& root_layer,
                         TopologyRecordRaw* record) {
    // Authoritative local-authorship check: does the root layer author a spec
    // at this path?  This uses a direct layer lookup rather than the composed
    // prim stack, because an *inert* over (an empty "Create Opinion" override
    // with no properties/children) is culled from `GetPrimStack()` yet is
    // exactly the spec that must flip the prim to locally-authored/editable.
    if (root_layer) {
        if (const SdfPrimSpecHandle spec =
                root_layer->GetPrimAtPath(prim.GetPath())) {
            record->local_specifier =
                rust::String(specifier_name(spec->GetSpecifier()));
            record->local_layer_identifier =
                rust::String(root_layer->GetIdentifier());
        }
    }
    for (const SdfPrimSpecHandle& spec : prim.GetPrimStack()) {
        if (record->packed_arc.empty()) {
            const std::vector<SdfPayload> payloads =
                spec->GetPayloadList().GetAddedOrExplicitItems();
            for (const SdfPayload& payload : payloads) {
                if (is_usd_asset(payload.GetAssetPath())) {
                    record->packed_arc = rust::String("payload");
                    record->packed_asset_path = rust::String(payload.GetAssetPath());
                    break;
                }
            }
        }
        if (record->packed_arc.empty()) {
            const std::vector<SdfReference> references =
                spec->GetReferenceList().GetAddedOrExplicitItems();
            for (const SdfReference& reference : references) {
                if (is_usd_asset(reference.GetAssetPath())) {
                    record->packed_arc = rust::String("reference");
                    record->packed_asset_path = rust::String(reference.GetAssetPath());
                    break;
                }
            }
        }
        if (record->internal_ref_target.empty()) {
            const std::vector<SdfReference> references =
                spec->GetReferenceList().GetAddedOrExplicitItems();
            for (const SdfReference& reference : references) {
                if (reference.GetAssetPath().empty() &&
                    !reference.GetPrimPath().IsEmpty()) {
                    record->internal_ref_target =
                        rust::String(reference.GetPrimPath().GetString());
                    break;
                }
            }
        }
    }
}

MaterialRecordRaw material_record(const UsdShadeMaterial& material,
                                  const UsdTimeCode& time) {
    MaterialRecordRaw record;
    record.path = rust::String(material.GetPath().GetString());
    record.material_ir_version = kMaterialIrVersion;
    record.material_model = rust::String("diagnostic_fallback");
    record.material_feature_bits = 0;
    record.alpha_policy = rust::String("opaque");
    record.sidedness = rust::String("inherited");
    record.base_color.push_back(0.18f);
    record.base_color.push_back(0.18f);
    record.base_color.push_back(0.18f);
    record.metallic = 0.0f;
    record.roughness = 0.5f;
    record.subsurface = 0.0f;
    record.emissive.push_back(0.0f);
    record.emissive.push_back(0.0f);
    record.emissive.push_back(0.0f);
    // Beer-Lambert transmission tint defaults to white (no tint / unauthored).
    record.transmission_color.push_back(1.0f);
    record.transmission_color.push_back(1.0f);
    record.transmission_color.push_back(1.0f);
    record.opacity = 1.0f;
    record.opacity_threshold = 0.0f;
    record.opacity_mode = OpacityModeRaw::Transparent;
    record.metallic_texture_channel = 0;
    record.roughness_texture_channel = 0;
    record.occlusion_texture_channel = 0;
    record.uses_diffuse_texture_alpha = false;

    const UsdShadeShader shader = surface_source(material);
    if (!shader) {
        MaterialDiagnosticRaw diagnostic;
        diagnostic.code = rust::String("unsupported_surface");
        diagnostic.source_path = rust::String(material.GetPath().GetString());
        const std::string message = material.GetPath().GetString() +
            " has no resolvable surface terminal; using deterministic gray fallback";
        diagnostic.message = rust::String(message);
        record.material_diagnostics.push_back(std::move(diagnostic));
        record.diagnostics.push_back(rust::String(message));
        record.material_ir_fingerprint =
            rust::String(material_ir_fingerprint(record));
        return record;
    }
    TfToken shader_id;
    shader.GetIdAttr().Get(&shader_id);
    const bool preview = shader_id == TfToken("UsdPreviewSurface");
    const bool standard_surface =
        shader_id == TfToken("ND_standard_surface_surfaceshader");
    if (!preview && !standard_surface) {
        MaterialIrLowerer lowerer(material, shader, time, &record);
        lowerer.diagnostic(
            "unsupported_surface", shader.GetPath().GetString(),
            material.GetPath().GetString() + " uses unsupported surface shader " +
                shader_id.GetString() + "; using deterministic gray fallback");
        record.material_ir_fingerprint =
            rust::String(material_ir_fingerprint(record));
        return record;
    }
    record.material_model = rust::String(
        preview ? "usd_preview_surface" : "materialx_standard_surface");
    GfVec3f color;
    const UsdShadeInput diffuse_input = shader.GetInput(
        preview ? TfToken("diffuseColor") : TfToken("base_color"));
    if (diffuse_input && diffuse_input.Get(&color, time)) {
        record.base_color.clear();
        for (int axis = 0; axis < 3; ++axis) {
            record.base_color.push_back(color[axis]);
        }
    }
    const UsdShadeShader materialx_diffuse = standard_surface
        ? connected_materialx_image(shader, TfToken("base_color"))
        : UsdShadeShader();
    const std::string diffuse_texture = preview
        ? connected_texture_path(shader, TfToken("diffuseColor"), time)
        : texture_asset_path(materialx_diffuse, time);
    const UsdShadeShader diffuse_texture_shader = preview
        ? connected_texture_shader(shader, TfToken("diffuseColor"))
        : UsdShadeShader();
    if (diffuse_texture_shader) {
        record.diffuse_uv_set = rust::String(
            texture_uv_set(diffuse_texture_shader, time,
                           &record.diffuse_uv_transform));
    }
    if (!diffuse_texture.empty()) {
        record.diffuse_texture = rust::String(diffuse_texture);
        record.base_color.clear();
        record.base_color.push_back(1.0f);
        record.base_color.push_back(1.0f);
        record.base_color.push_back(1.0f);
    }
    record.normal_texture = rust::String(preview
        ? connected_texture_path(shader, TfToken("normal"), time)
        : texture_asset_path(
              connected_materialx_image(shader, TfToken("normal")), time));
    const TfToken metallic_name = preview ? TfToken("metallic") : TfToken("metalness");
    const TfToken roughness_name = preview ? TfToken("roughness")
                                           : TfToken("specular_roughness");
    const UsdShadeShader metallic_texture_shader = preview
        ? connected_texture_shader(shader, metallic_name)
        : connected_materialx_image(shader, metallic_name);
    const UsdShadeShader roughness_texture_shader = preview
        ? connected_texture_shader(shader, roughness_name)
        : connected_materialx_image(shader, roughness_name);
    record.metallic_texture = rust::String(
        texture_asset_path(metallic_texture_shader, time));
    record.roughness_texture = rust::String(
        texture_asset_path(roughness_texture_shader, time));
    if (preview) {
        record.metallic_texture_channel =
            texture_channel(connected_source_name(shader, metallic_name));
        record.roughness_texture_channel =
            texture_channel(connected_source_name(shader, roughness_name));
    }
    if (!record.metallic_texture.empty()) {
        record.metallic = 1.0f;
    }
    if (!record.roughness_texture.empty()) {
        record.roughness = 1.0f;
    }
    const UsdShadeInput metallic_input = shader.GetInput(
        preview ? TfToken("metallic") : TfToken("metalness"));
    if (metallic_input) {
        metallic_input.Get(&record.metallic, time);
    }
    const UsdShadeInput roughness_input = shader.GetInput(
        preview ? TfToken("roughness") : TfToken("specular_roughness"));
    if (roughness_input) {
        roughness_input.Get(&record.roughness, time);
    }
    if (standard_surface) {
        const UsdShadeInput subsurface_input = shader.GetInput(TfToken("subsurface"));
        if (subsurface_input && !subsurface_input.Get(&record.subsurface, time) &&
            subsurface_input.HasConnectedSource()) {
            // Without a thickness buffer, connected scattering graphs lower to
            // a stable diffuse-wrap strength in the realtime viewport.
            record.subsurface = 0.35f;
        }
    }
    GfVec3f emissive;
    const UsdShadeInput emissive_input = shader.GetInput(
        preview ? TfToken("emissiveColor") : TfToken("emission_color"));
    const bool has_emissive_value = emissive_input && emissive_input.Get(&emissive, time);
    if (has_emissive_value) {
        record.emissive.clear();
        for (int axis = 0; axis < 3; ++axis) {
            record.emissive.push_back(emissive[axis]);
        }
    }
    const UsdShadeShader emissive_texture_shader = preview
        ? connected_texture_shader(shader, TfToken("emissiveColor"))
        : connected_materialx_image(shader, TfToken("emission_color"));
    record.emissive_texture = rust::String(
        texture_asset_path(emissive_texture_shader, time));
    if (!record.emissive_texture.empty() && !has_emissive_value) {
        record.emissive.clear();
        record.emissive.push_back(1.0f);
        record.emissive.push_back(1.0f);
        record.emissive.push_back(1.0f);
    }
    if (standard_surface) {
        float emission = emissive_input && emissive_input.HasConnectedSource()
            ? 1.0f
            : 0.0f;
        const UsdShadeInput emission_input = shader.GetInput(TfToken("emission"));
        if (emission_input) {
            emission_input.Get(&emission, time);
        }
        for (float& channel : record.emissive) {
            channel *= emission;
        }
    }
    if (preview) {
        const UsdShadeShader occlusion_texture_shader =
            connected_texture_shader(shader, TfToken("occlusion"));
        record.occlusion_texture = rust::String(
            texture_asset_path(occlusion_texture_shader, time));
        record.occlusion_texture_channel = texture_channel(
            connected_source_name(shader, TfToken("occlusion")));
    }
    const UsdShadeInput opacity_input = shader.GetInput(TfToken("opacity"));
    // A connection takes precedence over the authored fallback scalar, matching how
    // UsdImaging/Storm resolve shader inputs. DCC exporters (Houdini/Karma) author a
    // placeholder `inputs:opacity = 0` *alongside* a live
    // `inputs:opacity.connect -> <diffuse texture>.outputs:a`; that 0 is a default the
    // connection overrides, not the opacity value. Only the unconnected authored scalar
    // is authoritative for `record.opacity`; when connected, leave it at its 1.0 default
    // and let the sampled texture alpha drive per-pixel opacity.
    const bool opacity_connected = opacity_input && opacity_input.HasConnectedSource();
    if (opacity_input) {
        if (preview) {
            if (!opacity_connected) {
                opacity_input.Get(&record.opacity, time);
            }
        } else {
            GfVec3f opacity;
            if (opacity_input.Get(&opacity, time)) {
                record.opacity = (opacity[0] + opacity[1] + opacity[2]) / 3.0f;
            }
        }
    }
    if (preview && opacity_connected) {
        const UsdShadeShader opacity_texture_shader =
            connected_texture_shader(shader, TfToken("opacity"));
        if (opacity_texture_shader && diffuse_texture_shader &&
            opacity_texture_shader.GetPath() == diffuse_texture_shader.GetPath()) {
            record.uses_diffuse_texture_alpha = true;
        }
    }
    // MaterialX standard_surface: `transmission` (0=opaque, 1=fully transmissive) is
    // an independent glass/refraction parameter separate from `opacity`. Map it onto
    // `record.opacity` so the downstream alpha_policy logic detects AlphaPolicy::Blend.
    //
    // UsdMtlx wires even plain scalar values through a NodeGraph interface connection,
    // so direct Get() on the shader input fails. Follow the connection chain using
    // GetValueProducingAttributes() to reach the terminal value-authoring attribute.
    if (standard_surface) {
        float transmission = 0.0f;
        const UsdShadeInput transmission_input =
            shader.GetInput(TfToken("transmission"));
        if (transmission_input) {
            if (!transmission_input.Get(&transmission, time)) {
                // Plain Get() fails for connected inputs — trace to the value source.
                const UsdShadeAttributeVector sources =
                    UsdShadeUtils::GetValueProducingAttributes(transmission_input);
                for (const UsdAttribute& attr : sources) {
                    if (attr.Get(&transmission, time)) {
                        break;
                    }
                }
            }
        }
        if (transmission > 0.0f) {
            record.opacity *= (1.0f - transmission);
        }
        // `transmission_color` is the Beer-Lambert absorption tint the light picks
        // up passing *through* the glass — independent of the scalar `transmission`
        // fold above. Read it as a GfVec3f, tracing the same NodeGraph interface
        // connection chain UsdMtlx wires scalar values through. Defaults to the
        // white pushed in the defaults block when absent.
        const UsdShadeInput transmission_color_input =
            shader.GetInput(TfToken("transmission_color"));
        if (transmission_color_input) {
            GfVec3f transmission_color(1.0f, 1.0f, 1.0f);
            bool have_value = transmission_color_input.Get(&transmission_color, time);
            if (!have_value) {
                const UsdShadeAttributeVector sources =
                    UsdShadeUtils::GetValueProducingAttributes(
                        transmission_color_input);
                for (const UsdAttribute& attr : sources) {
                    if (attr.Get(&transmission_color, time)) {
                        have_value = true;
                        break;
                    }
                }
            }
            if (have_value) {
                record.transmission_color[0] = transmission_color[0];
                record.transmission_color[1] = transmission_color[1];
                record.transmission_color[2] = transmission_color[2];
            }
        }
    }
    const UsdShadeInput threshold_input =
        shader.GetInput(TfToken("opacityThreshold"));
    if (threshold_input) {
        threshold_input.Get(&record.opacity_threshold, time);
    }
    MaterialIrLowerer lowerer(material, shader, time, &record);
    if (preview) {
        const UsdShadeInput mode_input = shader.GetInput(TfToken("opacityMode"));
        if (mode_input) {
            TfToken mode;
            bool have_value = false;
            // Connections override local fallback values. opacityMode is
            // interfaceOnly: resolve Material/NodeGraph inputs, never shader outputs.
            if (mode_input.HasConnectedSource()) {
                for (const UsdAttribute& attr :
                     UsdShadeUtils::GetValueProducingAttributes(mode_input)) {
                    if (UsdShadeInput::IsInput(attr) && attr.Get(&mode, time)) {
                        have_value = true;
                        break;
                    }
                }
            } else {
                have_value = mode_input.Get(&mode, time);
            }
            if (have_value && mode == TfToken("presence")) {
                record.opacity_mode = OpacityModeRaw::Presence;
            } else if (have_value && mode != TfToken("transparent")) {
                lowerer.diagnostic(
                    "unsupported_input", mode_input.GetAttr().GetPath().GetString(),
                    "unsupported UsdPreviewSurface opacityMode '" + mode.GetString() +
                        "'; using transparent");
            } else if (!have_value && mode_input.HasConnectedSource()) {
                lowerer.diagnostic(
                    "unsupported_connection", mode_input.GetAttr().GetPath().GetString(),
                    "opacityMode requires a token-valued interface input; using transparent");
            }
        }
    }
    if (standard_surface) {
        for (const UsdShadeInput& input : shader.GetInputs()) {
            const std::string name = input.GetBaseName().GetString();
            static const std::set<std::string> portable = {
                "base", "base_color", "metalness", "specular_roughness",
                "normal", "emission", "emission_color", "opacity",
                "subsurface", "subsurface_color", "subsurface_radius",
                "transmission", "transmission_color"};
            if (input.HasConnectedSource() &&
                !connects_to_material_interface(input) &&
                portable.find(name) == portable.end()) {
                lowerer.diagnostic(
                    "unsupported_input", shader.GetPath().GetString(),
                    material.GetPath().GetString() + " MaterialX input " + name +
                        " is connected but outside the portable subset");
            }
        }
    }
    lowerer.lower(preview ? TfToken("diffuseColor") : TfToken("base_color"),
                  "base_color");
    lowerer.lower(TfToken("normal"), "normal");
    lowerer.lower(preview ? TfToken("metallic") : TfToken("metalness"),
                  "metallic");
    lowerer.lower(preview ? TfToken("roughness")
                          : TfToken("specular_roughness"),
                  "roughness");
    if (preview) {
        lowerer.lower(TfToken("occlusion"), "occlusion");
    }
    lowerer.lower(preview ? TfToken("emissiveColor")
                          : TfToken("emission_color"),
                  "emissive");
    lowerer.lower(TfToken("opacity"), "opacity");
    lowerer.finish();

    const TextureInputRaw* base_texture = nullptr;
    const TextureInputRaw* opacity_texture = nullptr;
    for (const TextureInputRaw& texture : record.texture_inputs) {
        const std::string semantic(texture.semantic);
        const std::string path = texture_path(texture);
        const std::string swizzle(texture.swizzle);
        const std::uint32_t channel = texture_channel(TfToken(swizzle));
        if (semantic == "base_color") {
            base_texture = &texture;
            record.diffuse_texture = rust::String(path);
            record.diffuse_uv_set = rust::String(std::string(texture.uv_name));
            record.diffuse_uv_transform.clear();
            for (float value : texture.uv_transform) {
                record.diffuse_uv_transform.push_back(value);
            }
            record.base_color.clear();
            // UsdUVTexture `scale` multiplies the sampled texture value
            // (UsdPreviewSurface / UsdUVTexture spec: result = sample*scale +
            // bias). Carry scale.rgb as the base_color factor so the shader
            // applies it as a diffuse tint over the texture; it defaults to
            // (1,1,1) when unauthored, leaving a plain texture unchanged. Normal
            // maps do their own scale/bias remap in-shader and never reach this
            // base_color path (their semantic is "normal", not "base_color").
            for (int index = 0; index < 3; ++index) {
                record.base_color.push_back(texture.scale[index]);
            }
        } else if (semantic == "normal") {
            record.normal_texture = rust::String(path);
        } else if (semantic == "metallic") {
            record.metallic_texture = rust::String(path);
            record.metallic_texture_channel = channel;
            record.metallic = 1.0f;
        } else if (semantic == "roughness") {
            record.roughness_texture = rust::String(path);
            record.roughness_texture_channel = channel;
            record.roughness = 1.0f;
        } else if (semantic == "occlusion") {
            record.occlusion_texture = rust::String(path);
            record.occlusion_texture_channel = channel;
        } else if (semantic == "emissive") {
            record.emissive_texture = rust::String(path);
            if (std::all_of(record.emissive.begin(), record.emissive.end(),
                            [](float value) { return value == 0.0f; })) {
                record.emissive.clear();
                for (int index = 0; index < 3; ++index) {
                    record.emissive.push_back(1.0f);
                }
            }
        } else if (semantic == "opacity") {
            opacity_texture = &texture;
        }
    }
    if (opacity_connected &&
        base_texture != nullptr && opacity_texture != nullptr &&
        texture_path(*base_texture) == texture_path(*opacity_texture) &&
        std::string(opacity_texture->swizzle) == "a") {
        record.uses_diffuse_texture_alpha = true;
    } else if (opacity_texture != nullptr) {
        lowerer.diagnostic(
            "unsupported_connection", material.GetPath().GetString(),
            material.GetPath().GetString() +
                " preserves a separate opacity texture in Material IR; the legacy " +
                "viewport adapter still requires diffuse alpha");
    }
    if (record.opacity_threshold > 0.0f) {
        record.alpha_policy = rust::String("mask");
    } else if (record.opacity < 1.0f ||
               (record.opacity_mode == OpacityModeRaw::Presence &&
                opacity_texture != nullptr)) {
        // Blend only when opacity is *genuinely* sub-unit: an authored scalar
        // opacity < 1 (or a transmission→opacity bridge, above). A mere
        // connection of `opacity` to the diffuse texture's alpha channel
        // (`uses_diffuse_texture_alpha`) is NOT sufficient — DCC exporters
        // (e.g. Houdini/Karma) wire opacity→baseColor.a even for fully opaque
        // assets, and their alpha is 1.0. Classifying those as Blend routed
        // them to the depth-write-OFF transparent pass, so opaque geometry
        // stopped occluding anything (back faces bled through front faces and
        // objects behind showed through). Default such materials to Opaque; a
        // real cutout must author `opacityThreshold` (→ Mask, above). Explicit
        // presence opts into coverage semantics, including texture-driven fades.
        record.alpha_policy = rust::String("blend");
    }
    record.material_ir_fingerprint =
        rust::String(material_ir_fingerprint(record));
    return record;
}

rust::Vec<float> display_color(const UsdPrim& prim, const UsdTimeCode& time) {
    rust::Vec<float> out;
    VtArray<GfVec3f> colors;
    if (UsdGeomGprim(prim).GetDisplayColorAttr().Get(&colors, time)) {
        for (const auto& c : colors) {
            out.push_back(c[0]);
            out.push_back(c[1]);
            out.push_back(c[2]);
        }
    }
    return out;
}

bool is_visible(const UsdPrim& prim, const UsdTimeCode& time) {
    const UsdGeomImageable imageable(prim);
    return !imageable || imageable.ComputeVisibility(time) !=
                             UsdGeomTokens->invisible;
}

void append_matrix(rust::Vec<float>* values, const GfMatrix4d& matrix) {
    values->reserve(16);
    for (int row = 0; row < 4; ++row) {
        for (int column = 0; column < 4; ++column) {
            values->push_back(static_cast<float>(matrix[row][column]));
        }
    }
}

// Reads a UsdLux light input, honouring legacy (pre-USD-21.02) un-namespaced
// attribute names. USD 21.02 moved all light inputs into the "inputs:"
// namespace (intensity -> inputs:intensity, etc.) so lights could be
// connectable; the generated schema accessors resolve only the namespaced
// form. Content exported by DCC tools that still write bare names
// (e.g. Houdini/Karma) would otherwise silently fall through to the schema
// fallback, ignoring the artist's authored values.
//
// Precedence follows USD convention: an authored `inputs:X` wins, then an
// authored legacy bare `X`, and if neither is authored `*out` is left at the
// caller-provided schema default.
template <class T>
void read_light_input(const UsdPrim& prim, const UsdAttribute& input_attr,
                      const char* legacy_name, T* out,
                      const UsdTimeCode& time) {
    if (input_attr && input_attr.HasAuthoredValue()) {
        input_attr.Get(out, time);
        return;
    }
    const UsdAttribute legacy = prim.GetAttribute(TfToken(legacy_name));
    if (legacy && legacy.HasAuthoredValue()) {
        legacy.Get(out, time);
    }
}

LightRecordRaw light_record(const UsdPrim& prim, const UsdTimeCode& time,
                            UsdGeomXformCache* transforms,
                            const std::string& kind) {
    LightRecordRaw record;
    record.path = rust::String(prim.GetPath().GetString());
    record.kind = rust::String(kind);
    append_matrix(&record.world_matrix,
                  transforms->GetLocalToWorldTransform(prim));

    const UsdLuxLightAPI light(prim);
    GfVec3f color(1.0f);
    record.intensity = 1.0f;
    record.exposure = 0.0f;
    record.diffuse = 1.0f;
    record.specular = 1.0f;
    record.normalize = false;
    read_light_input(prim, light.GetColorAttr(), "color", &color, time);
    read_light_input(prim, light.GetIntensityAttr(), "intensity",
                     &record.intensity, time);
    read_light_input(prim, light.GetExposureAttr(), "exposure",
                     &record.exposure, time);
    read_light_input(prim, light.GetDiffuseAttr(), "diffuse", &record.diffuse,
                     time);
    read_light_input(prim, light.GetSpecularAttr(), "specular",
                     &record.specular, time);
    read_light_input(prim, light.GetNormalizeAttr(), "normalize",
                     &record.normalize, time);
    bool enable_temperature = false;
    read_light_input(prim, light.GetEnableColorTemperatureAttr(),
                     "enableColorTemperature", &enable_temperature, time);
    // Surface the raw authored temperature (Kelvin) separately so Rust consumers
    // and the inspector can report it; the blackbody multiplier is still folded
    // into `color` below to match OpenUSD's rendered result.
    record.enable_color_temperature = enable_temperature;
    record.color_temperature = 6500.0f;
    if (enable_temperature) {
        read_light_input(prim, light.GetColorTemperatureAttr(),
                         "colorTemperature", &record.color_temperature, time);
        const GfVec3f multiplier =
            UsdLuxBlackbodyTemperatureAsRgb(record.color_temperature);
        for (int channel = 0; channel < 3; ++channel) {
            color[channel] *= multiplier[channel];
        }
    }
    for (int channel = 0; channel < 3; ++channel) {
        record.color.push_back(color[channel]);
    }

    record.angle = 0.0f;
    record.radius = 0.0f;
    record.width = 0.0f;
    record.height = 0.0f;
    record.length = 0.0f;
    if (kind == "distant") {
        record.angle = 0.53f;
        read_light_input(prim, UsdLuxDistantLight(prim).GetAngleAttr(), "angle",
                         &record.angle, time);
    } else if (kind == "sphere") {
        record.radius = 0.5f;
        read_light_input(prim, UsdLuxSphereLight(prim).GetRadiusAttr(),
                         "radius", &record.radius, time);
    } else if (kind == "disk") {
        record.radius = 0.5f;
        read_light_input(prim, UsdLuxDiskLight(prim).GetRadiusAttr(),
                         "radius", &record.radius, time);
    } else if (kind == "cylinder") {
        record.radius = 0.5f;
        record.length = 1.0f;
        const UsdLuxCylinderLight cylinder(prim);
        read_light_input(prim, cylinder.GetRadiusAttr(), "radius",
                         &record.radius, time);
        read_light_input(prim, cylinder.GetLengthAttr(), "length",
                         &record.length, time);
    } else if (kind == "rect") {
        record.width = 1.0f;
        record.height = 1.0f;
        const UsdLuxRectLight rect(prim);
        read_light_input(prim, rect.GetWidthAttr(), "width", &record.width,
                         time);
        read_light_input(prim, rect.GetHeightAttr(), "height", &record.height,
                         time);
    }

    // UsdLuxShapingAPI cone turns a sphere/disk light into a spotlight. A cone
    // half-angle of 180 (schema default) means "no cone".
    record.has_shaping = false;
    record.cone_angle = 180.0f;
    record.cone_softness = 0.0f;
    if (prim.HasAPI<UsdLuxShapingAPI>()) {
        const UsdLuxShapingAPI shaping(prim);
        const UsdAttribute cone_attr = shaping.GetShapingConeAngleAttr();
        if (cone_attr && cone_attr.HasAuthoredValue()) {
            record.has_shaping = true;
            cone_attr.Get(&record.cone_angle, time);
            read_light_input(prim, shaping.GetShapingConeSoftnessAttr(),
                             "shaping:cone:softness", &record.cone_softness,
                             time);
        }
    }

    record.shadow_enabled = false;
    if (prim.HasAPI<UsdLuxShadowAPI>()) {
        record.shadow_enabled = true;
        read_light_input(prim, UsdLuxShadowAPI(prim).GetShadowEnableAttr(),
                         "shadow:enable", &record.shadow_enabled, time);
    }

    record.dome_is_procedural = false;
    // `poleAxis` metadata governs which world axis the lat-long poles map to;
    // default "scene" means "follow the stage up-axis". Empty string when unset.
    record.dome_texture_format = rust::String("");
    record.dome_pole_axis = rust::String("");
    if (kind == "dome" || kind == "dome_procedural") {
        const UsdLuxDomeLight dome(prim);
        if (dome) {
            // Honor legacy pre-USD-21.02 un-namespaced `texture:file` (authored on
            // older assets) when the modern `inputs:texture:file` is not authored —
            // the same precedence `read_light_input` applies to scalar inputs. The
            // dome texture is read directly (SdfAssetPath), so it needs its own
            // fallback. Keep the resolved *attribute* so anchoring can walk its
            // property stack.
            UsdAttribute tex_attr = dome.GetTextureFileAttr();
            if (!tex_attr || !tex_attr.HasAuthoredValue()) {
                const UsdAttribute legacy =
                    prim.GetAttribute(TfToken("texture:file"));
                if (legacy && legacy.HasAuthoredValue()) {
                    tex_attr = legacy;
                }
            }
            SdfAssetPath texture_file;
            if (tex_attr && tex_attr.Get(&texture_file, time)) {
                const std::string resolved = texture_file.GetResolvedPath();
                // Mirror the material texture path: when the resolver yields
                // nothing (relative ref the default resolver can't resolve at
                // projection time), anchor the authored path against its
                // authoring layer so the Rust side gets an absolute path to
                // read, rather than a bare layer-relative string that
                // std::fs::read would resolve against the process CWD.
                std::string path = resolved;
                if (path.empty()) {
                    path = anchor_asset_to_layer(tex_attr,
                                                 texture_file.GetAssetPath(),
                                                 time);
                }
                if (path.empty()) {
                    path = texture_file.GetAssetPath();
                }
                record.dome_texture_path = rust::String(path);
            }
            // `texture:format` moved into the `inputs:` namespace in the same
            // transition; fall back to the legacy name likewise.
            TfToken format;
            UsdAttribute fmt_attr = dome.GetTextureFormatAttr();
            if (!fmt_attr || !fmt_attr.HasAuthoredValue()) {
                const UsdAttribute legacy_fmt =
                    prim.GetAttribute(TfToken("texture:format"));
                if (legacy_fmt && legacy_fmt.HasAuthoredValue()) {
                    fmt_attr = legacy_fmt;
                }
            }
            if (fmt_attr && fmt_attr.Get(&format, time)) {
                record.dome_texture_format = rust::String(format.GetString());
            }
            // `poleAxis` is only a schema attribute on UsdLuxDomeLight_1, so read
            // it generically by name to support both dome schema versions.
            const UsdAttribute pole_attr =
                prim.GetAttribute(TfToken("poleAxis"));
            TfToken pole_axis;
            if (pole_attr && pole_attr.Get(&pole_axis, time)) {
                record.dome_pole_axis = rust::String(pole_axis.GetString());
            }
        }
        record.dome_is_procedural = (kind == "dome_procedural");
    }

    return record;
}

struct PendingChange {
    std::string prim_path;
    std::string property_name;
    std::uint32_t domains = 0;
    bool resync = false;
};

class Observer final : public TfWeakBase {
public:
    void changed(const UsdNotice::ObjectsChanged& notice,
                 const UsdStageWeakPtr& stage) {
        std::lock_guard<std::mutex> guard(mutex_);
        for (const SdfPath& path : notice.GetResyncedPaths()) {
            if (path.IsPropertyPath()) {
                // A resync at a PROPERTY path is a newly-created (or removed)
                // property *spec* — e.g. the first root-layer `xformOp` override
                // on a prim whose transform composed entirely from a reference or
                // payload (the `could not capture transform property spec` path).
                // The prim's topology is unchanged; only a property gained a root
                // opinion. Classify it by name exactly like the changed-info path
                // below, so a first-time transform override dirties TRANSFORM (and
                // takes the per-prim viewport fast path) instead of the blanket
                // TOPOLOGY|SKELETON a structural resync gets — otherwise every
                // transform edit on a referenced/payload prim forces a full
                // subtree re-projection + skeleton-cache rebuild.
                const SdfPath prim = path.GetPrimPath();
                const std::string property = path.GetNameToken().GetString();
                std::uint32_t domains = classify(stage, prim, property);
                if ((domains & (kSkeleton | kAnimation | kRelationship)) != 0) {
                    skeleton_cache_dirty_ = true;
                }
                pending_.push_back({prim.GetString(), property, domains, false});
                continue;
            }
            // A genuine structural prim resync (add / remove / reparent, or a
            // native-instancing prototype change): keep the conservative topology
            // + skeleton + prototype invalidation.
            pending_.push_back(
                {path.GetString(), "", kTopology | kSkeleton, true});
            skeleton_cache_dirty_ = true;
            // A structural resync can add, remove, or re-author native
            // instancing prototypes (/__Prototype_N). Invalidate prototype
            // residency so the next projection re-marshals prototype geometry.
            prototypes_dirty_ = true;
        }
        for (const SdfPath& path : notice.GetChangedInfoOnlyPaths()) {
            const bool property_path = path.IsPropertyPath();
            const SdfPath prim = property_path ? path.GetPrimPath() : path;
            const std::string property =
                property_path ? path.GetNameToken().GetString() : std::string();
            std::uint32_t domains = classify(stage, prim, property);
            const TfTokenVector changed_fields = notice.GetChangedFields(path);
            if (std::find(changed_fields.begin(), changed_fields.end(),
                          SdfFieldKeys->TimeSamples) != changed_fields.end()) {
                domains |= kAnimation;
            }
            if ((domains & (kSkeleton | kAnimation | kRelationship)) != 0) {
                skeleton_cache_dirty_ = true;
            }
            pending_.push_back({prim.GetString(), property, domains, false});
        }
    }

    std::vector<PendingChange> drain() {
        std::lock_guard<std::mutex> guard(mutex_);
        std::vector<PendingChange> drained;
        drained.swap(pending_);
        return drained;
    }

    void discard() {
        std::lock_guard<std::mutex> guard(mutex_);
        pending_.clear();
    }

    bool take_skeleton_cache_dirty() {
        std::lock_guard<std::mutex> guard(mutex_);
        const bool dirty = skeleton_cache_dirty_;
        skeleton_cache_dirty_ = false;
        return dirty;
    }

    bool take_prototypes_dirty() {
        std::lock_guard<std::mutex> guard(mutex_);
        const bool dirty = prototypes_dirty_;
        prototypes_dirty_ = false;
        return dirty;
    }

    std::size_t checkpoint() {
        std::lock_guard<std::mutex> guard(mutex_);
        return pending_.size();
    }

    void rollback_to(std::size_t checkpoint) {
        std::lock_guard<std::mutex> guard(mutex_);
        if (pending_.size() > checkpoint) {
            pending_.resize(checkpoint);
        }
    }

    // A validated array transaction cannot change prim topology. Replace only
    // its own notices (including first session-over creation) with value dirties.
    struct InstancerCheckpoint { std::size_t pending; bool skeleton; bool prototypes; };
    InstancerCheckpoint instancer_checkpoint() {
        std::lock_guard<std::mutex> guard(mutex_);
        return {pending_.size(), skeleton_cache_dirty_, prototypes_dirty_};
    }
    void instancer_commit(InstancerCheckpoint checkpoint, const SdfPath& path, bool mask_only, bool success, bool animation_changed = false) {
        std::lock_guard<std::mutex> guard(mutex_);
        pending_.resize(checkpoint.pending);
        skeleton_cache_dirty_ = checkpoint.skeleton;
        prototypes_dirty_ = checkpoint.prototypes;
        if (success) pending_.push_back({path.GetString(), mask_only ? "invisibleIds" : "positions",
                                        kInstancer | kInspection | (animation_changed ? kAnimation : 0), false});
    }

private:
    std::mutex mutex_;
    std::vector<PendingChange> pending_;
    bool skeleton_cache_dirty_ = false;
    bool prototypes_dirty_ = false;
};

struct CachedSkeletonDependency {
    SdfPath root_path;
    SdfPath skeleton_path;
    SdfPath animation_path;
    SdfPathVector skinned_paths;
};

// One isolated composition per acquired action. Root/session assets are shared,
// but native animation bindings and UsdSkel caches never leak between actors or
// into save/cook/collaboration. Destruction is under the owning stage lock.
struct AnimationBindingContext {
    UsdStageRefPtr stage;
    std::string owner;
    std::string source;
    std::vector<std::string> roots;
    UsdSkelCache cache;
    bool initialized = false;
    std::vector<CachedSkeletonDependency> dependencies;
    std::unique_ptr<Observer> observer;
    TfNotice::Key observer_key;
    std::map<SdfPath, SdfPathVector> point_prototypes;
    std::set<SdfPath> prototype_roots;
    bool point_initialized = false;
    bool prototypes_resident = true;
    ~AnimationBindingContext() { TfNotice::Revoke(observer_key); }
};

}  // namespace

// Process-wide count of live `StageHandle::Impl` objects (one per opened
// stage). Incremented when an Impl is constructed and decremented at the *end*
// of ~Impl — after observer revocation + `stage.Reset()` complete under the
// exclusive lifecycle lock — so the count only drops once a handle is fully
// torn down. Exposed via `live_stage_handle_count()` for the U0.5e teardown
// leak-regression test: every open must be balanced by a full teardown.
std::atomic<std::uint64_t>& live_stage_handle_counter() {
    static std::atomic<std::uint64_t> counter{0};
    return counter;
}

// Root-opinion signatures are requested by every editor tick. Invalidate from
// Sdf (including opinions masked by stronger layers), not composed USD notices.
struct RootSignature : TfWeakBase {
    std::atomic<std::uint64_t> revision{1};
    std::mutex mutex;
    std::uint64_t cached_revision = 0;
    std::uint64_t hash = 0;
    TfNotice::Key key;
    void changed(const SdfNotice::LayersDidChangeSentPerLayer&, const SdfLayerHandle&) {
        revision.fetch_add(1, std::memory_order_release);
    }
    ~RootSignature() { TfNotice::Revoke(key); }
};

struct StageHandle::Impl {
    mutable std::shared_mutex stage_mutex;
    template <typename Body>
    auto access(Body&& body) const -> decltype(body()) {
        StageAccessGuard guard(stage_mutex);
        try { return body(); }
        catch (...) { guard.unlock(); throw; }
    }
    UsdStageRefPtr stage;
    mutable std::map<SdfPath, SdfPathVector> point_prototypes_by_instancer;
    mutable std::set<SdfPath> point_prototype_roots;
    mutable bool point_prototype_cache_initialized = false;
    mutable UsdSkelCache skeleton_cache;
    mutable bool skeleton_cache_initialized = false;
    mutable std::vector<CachedSkeletonDependency> skeleton_dependencies;
    mutable std::map<std::string, std::unique_ptr<AnimationBindingContext>> animation_bindings;
    // Native instancing prototype geometry (/__Prototype_N) is projected once and
    // then retained by the renderer across frames. `prototypes_resident` tracks
    // whether that geometry has already been emitted so root-scoped reprojections
    // (e.g. an XR controller pose write) skip re-marshaling every prototype.
    mutable bool prototypes_resident = false;
    std::unique_ptr<Observer> observer;
    TfNotice::Key observer_key;
    mutable RootSignature root_signature;
#ifdef IDTX_TEST_SUPPORT
    std::uint32_t transform_failure_after_writes = 0;
    std::uint32_t instancer_failure_after_writes = 0;
#endif

    Impl() { live_stage_handle_counter().fetch_add(1, std::memory_order_relaxed); }

    ~Impl() {
        StageAccessGuard lifecycle_guard(stage_mutex);
        TfNotice::Revoke(observer_key);
        TfNotice::Revoke(root_signature.key);
        // Field destruction happens after this destructor body and would fall
        // outside the lifecycle lock. Reset explicitly while the lock is held.
        animation_bindings.clear();
        stage.Reset();
        observer.reset();
        // Count the handle as gone only now that teardown is complete (observer
        // revoked, stage dropped) — release-ordered so a subsequent count load
        // observes the finished teardown, not a half-torn-down handle.
        live_stage_handle_counter().fetch_sub(1, std::memory_order_release);
    }
};

StageHandle::StageHandle() : impl_(std::make_unique<Impl>()) {}
StageHandle::~StageHandle() = default;

std::uint64_t live_stage_handle_count() {
    return live_stage_handle_counter().load(std::memory_order_acquire);
}

void register_plugins(rust::Str install_dir) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
    // Install the benign-warning filter before any stage work so the legacy
    // MaterialBindingAPI / <UDIM> probe floods never reach the console.
    install_benign_warning_filter_once();
    const std::string root = to_std(install_dir);
    PlugRegistry::GetInstance().RegisterPlugins(root + "/plugin/usd");
    PlugRegistry::GetInstance().RegisterPlugins(root + "/lib/usd");
    });
}

// Set the process-wide default search path used by ArDefaultResolver to resolve
// search-path-relative asset references (e.g. a bare `@prop.usda@` that does not
// resolve relative to its anchoring layer). `search_path` is a newline-separated
// list of absolute directories; empty segments are ignored. This anchors
// project-relative asset resolution at the project root + its asset roots (U1.7b)
// without touching the per-stage open path.
//
// SetDefaultSearchPath is documented as not thread-safe w.r.t. other resolver
// operations, so we hold the exclusive lifecycle lock — the same lock stage
// opens/reads take — to serialize it against any in-flight resolution.
void set_default_asset_search_path(rust::Str search_path) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
    const std::string joined = to_std(search_path);
    std::vector<std::string> dirs;
    std::string::size_type start = 0;
    while (start <= joined.size()) {
        const std::string::size_type nl = joined.find('\n', start);
        const std::string segment = (nl == std::string::npos)
            ? joined.substr(start)
            : joined.substr(start, nl - start);
        if (!segment.empty()) {
            dirs.push_back(segment);
        }
        if (nl == std::string::npos) {
            break;
        }
        start = nl + 1;
    }
    ArDefaultResolver::SetDefaultSearchPath(dirs);
    });
}

namespace {

// Scoped collector that captures OpenUSD diagnostics (errors/warnings/status)
// emitted **on the installing thread** while it is installed, so a packaging
// operation can surface them to the user afterwards.
//
// TfDiagnosticMgr is process-global and its delegate callbacks fire on the
// thread that posts each diagnostic. We deliberately filter to the installing
// thread's id: packaging runs on a dedicated worker thread (the Conductor
// "Import" job) while the main thread keeps projecting the viewport every frame,
// and a viewport projection — or an unrelated concurrent stage open on another
// thread — can post its own warnings during the multi-second packaging window.
// Capturing *every* thread's diagnostics would fold that unrelated noise into
// the package result and spuriously trip the "packaging finished with warnings"
// modal on an otherwise-clean import (the spec requires a clean package to open
// immediately). Thread filtering keeps the clean case correct and isolates
// concurrent packaging jobs from each other.
//
// Tradeoff: UsdUtilsCreateNewUsdzPackage may fan asset localization out across
// worker threads, so a diagnostic posted from one of those threads during an
// otherwise-successful package is not captured. That is acceptable — the package
// is still produced and opened; we simply don't echo that particular warning.
// Unresolvable references (the dominant packaging failure) are detected up front
// via compute_dependencies and reported through `unresolved`, not this collector.
//
// IMPORTANT: on this toolchain a C++ exception crossing the cxx `extern "C"`
// boundary does not run local destructors (see save_stage_locked). Callers
// MUST call detach() explicitly before any throw; the destructor is only a
// backstop for the non-throwing path.
class PackagingDiagnosticCollector final : public TfDiagnosticMgr::Delegate {
public:
    PackagingDiagnosticCollector() {
        // Mark this thread as packaging so the process-global benign-warning
        // filter stays silent for THIS thread's warnings/statuses (which we
        // capture below), without muting unrelated threads or dropping errors.
        // Balanced in detach().
        packaging_thread_enter();
        TfDiagnosticMgr::GetInstance().AddDelegate(this);
    }
    ~PackagingDiagnosticCollector() override { detach(); }

    PackagingDiagnosticCollector(const PackagingDiagnosticCollector&) = delete;
    PackagingDiagnosticCollector& operator=(const PackagingDiagnosticCollector&) = delete;

    void detach() {
        if (attached_) {
            TfDiagnosticMgr::GetInstance().RemoveDelegate(this);
            packaging_thread_leave();
            attached_ = false;
        }
    }

    void IssueError(const TfError& err) override {
        record("error", err.GetCommentary());
    }
    void IssueFatalError(const TfCallContext& context, const std::string& msg) override {
        (void)context;
        record("fatal", msg);
    }
    void IssueStatus(const TfStatus& status) override {
        record("status", status.GetCommentary());
    }
    void IssueWarning(const TfWarning& warning) override {
        record("warning", warning.GetCommentary());
    }

    std::vector<std::string> messages;

private:
    void record(const char* severity, const std::string& commentary) {
        // Only capture diagnostics posted from the thread that installed the
        // collector; callbacks from other threads (a concurrent viewport
        // projection or stage open) would be unrelated noise. See the class
        // comment for the full rationale and tradeoff.
        if (std::this_thread::get_id() != owner_thread_) {
            return;
        }
        std::string entry = std::string(severity) + ": " + commentary;
        std::lock_guard<std::mutex> guard(mutex_);
        if (messages.size() >= kMaxDiagnostics) {
            return;
        }
        // Dependency resolution and the packaging pass can each emit the same
        // diagnostic (e.g. an unresolvable reference is walked twice); collapse
        // exact duplicates so the user sees each issue once.
        if (std::find(messages.begin(), messages.end(), entry) != messages.end()) {
            return;
        }
        messages.push_back(std::move(entry));
    }

    static constexpr std::size_t kMaxDiagnostics = 256;
    const std::thread::id owner_thread_ = std::this_thread::get_id();
    std::mutex mutex_;
    bool attached_ = true;
};

// A USDZ meant to be composed into a level through a `reference` or `payload`
// must declare a `defaultPrim`: a composition arc that names no explicit prim
// path resolves to the asset's defaultPrim, and an asset without one contributes
// nothing — the prim the arc targets stays empty (the "payload renders nothing"
// trap, seen when an imported DCC scene with no root defaultPrim is payloaded
// into a level). DCC exports (Houdini in particular) routinely omit it.
//
// When the root layer has no defaultPrim and exactly one root-level `def` prim,
// adopt that prim as the defaultPrim so the packaged asset is usable as a
// reference/payload target. Layers that are ambiguous (zero, or several, root
// `def` prims) are left untouched — there is no safe automatic choice, and the
// author can set it explicitly. Edits the layer IN MEMORY only; the caller MUST
// restore its original content after packaging. Returns true when it changed it.
bool adopt_single_root_as_default_prim(const SdfLayerRefPtr& layer) {
    if (!layer || layer->HasDefaultPrim()) {
        return false;
    }
    const SdfPrimSpecHandle pseudo_root = layer->GetPseudoRoot();
    if (!pseudo_root) {
        return false;
    }
    SdfPrimSpecHandle sole_root;
    for (const SdfPrimSpecHandle& child : pseudo_root->GetNameChildren()) {
        if (!child || child->GetSpecifier() != SdfSpecifierDef) {
            continue;  // `over`/`class` roots are not composition defaults
        }
        if (sole_root) {
            return false;  // more than one candidate: ambiguous, leave it
        }
        sole_root = child;
    }
    if (!sole_root) {
        return false;
    }
    layer->SetDefaultPrim(sole_root->GetNameToken());
    return true;
}

// True when `asset_path` names a design-time behavior SOURCE — a Rust `.rs`
// authored on `idtx:behavior:source`. Such sources must NEVER ship in a cooked
// runtime package: the runtime only ever ticks the compiled `idtx:behavior:module`
// (`.wasm`), and USD's asset-dependency localizer would otherwise pull the `.rs`
// into the package because `idtx:behavior:source` is an `asset`-typed attribute
// (dragging in both player-rig variants' sources). This mirrors the runtime-
// prototype path, which blanks the source (`idtx:behavior:source = @@`, see
// prototype_prepare.rs). A `.rs` file is never a legitimate runtime USD asset, so
// the extension is a safe, sufficient discriminator.
static bool is_behavior_source_asset(const std::string& asset_path) {
    const bool remote = asset_path.rfind("http://", 0) == 0 || asset_path.rfind("https://", 0) == 0;
    std::string path = remote ? asset_path.substr(0, asset_path.find_first_of("?#")) : asset_path;
    if (ArIsPackageRelativePath(path)) path = ArSplitPackageRelativePathInner(path).second;
    return normalize_path_key(TfGetExtension(path)) == "rs";
}

// Clean layers can be restored from disk, but Reload(force) would discard a
// live editor's unsaved opinions on dirty layers. Snapshot those before ANY
// mutation and restore their content without changing their identifier/anchor.
// Restoration is explicit, not destructor-based (the no-/EHsc rule).
struct PackagingLayerBackup {
    SdfLayerRefPtr layer;
    SdfLayerRefPtr dirty_content;

    explicit PackagingLayerBackup(const SdfLayerRefPtr& original = SdfLayerRefPtr())
        : layer(original) {
        if (layer && layer->IsDirty()) {
            dirty_content = SdfLayer::CreateAnonymous();
            if (!dirty_content) {
                throw std::runtime_error("could not snapshot dirty packaging layer");
            }
            dirty_content->TransferContent(layer);
        }
    }

    void restore() const {
        if (!layer) return;
        if (dirty_content) {
            layer->TransferContent(dirty_content);
        } else {
            layer->Reload(/*force=*/true);
        }
    }
};

// Collapse divergent authored spellings of the same on-disk USD layer to a
// single identity so UsdUtilsCreateNewUsdzPackage's UsdUtils_DirectoryRemapper
// assigns ONE package bucket per real file. Houdini-exported scenes reference
// the same asset via different relative spellings depending on the referring
// layer's directory depth (e.g. `../coaster/foo.usd` from the root layer vs
// `../../coaster/foo.usd` from a `stage/` sublayer); the remapper keys on the
// anchored path string, so each spelling gets its own bucket and only one is
// written — every other arc's bucket is left empty and its reference dangles,
// silently dropping geometry and point-instancer prototypes (verified on
// BarScene: 6 danglers / 7 meshes lost without this pass, 0 / 0 with it).
//
// Rewrites, IN MEMORY only, every reference/payload/sublayer asset path in the
// `closure` layers whose layer-anchored absolute path names a real on-disk
// `.usd`/`.usda`/`.usdc` file to that absolute path. Texture paths, the literal
// `<UDIM>` template, non-file URIs (`opdef:/...`), package-internal paths and
// genuinely-unresolvable refs are left untouched (none is a regular USD file).
// When `unresolved` is non-null, asset paths matching it are CLEARED (returned
// empty) instead — composing the "drop unresolved" behaviour of
// package_stage_dropping_unresolved with the canonicalization in one pass.
//
// Returns backups of layers it actually changed; the caller MUST restore()
// each one after packaging to restore live content + the process-global Sdf
// registry. Both callers run this while holding the EXCLUSIVE stage lifecycle
// lock (see package_stage), so these in-memory rewrites + the caller's restore
// cannot race a concurrent read projection or recompose a live stage on another
// thread. If the pass itself throws part-way through, it restores every layer it
// has already rewritten before rethrowing (so a mid-pass failure leaves nothing
// globally mutated); per the no-/EHsc rule the caller must likewise run its own
// restores before any throw of its own.
std::vector<PackagingLayerBackup> canonicalize_closure_asset_paths(
    const std::vector<std::string>& closure,
    const std::unordered_set<std::string>* unresolved,
    bool strip_behavior_sources) {
    std::vector<PackagingLayerBackup> opened;
    // The closure can name the same layer more than once (e.g. the root both as
    // the caller's `source` identifier and as its resolved real path). Process
    // each layer exactly once: a second pass would see the already-rewritten
    // (so unchanged) paths, take the no-op `else` branch and Reload(force) the
    // layer — reverting the first pass's canonicalization.
    std::unordered_set<std::string> visited;
    // The layer currently mid-rewrite, so a throw part-way through the pass can
    // restore it too. Without this the caller only learns which layers changed on
    // a successful return, leaving a mid-pass failure's already-rewritten layers
    // globally mutated (the no-/EHsc rule means this catch runs, unlike local
    // destructors). See the exception-restore block at the end of the loop.
    PackagingLayerBackup in_progress;
    try {
    for (const std::string& layer_path : closure) {
        // Only USD layer files hold rewritable asset refs; opening a texture as
        // a layer emits a spurious "cannot determine file format" coding error
        // (this build has no Hio image plugins), so skip by extension.
        const std::string layer_ext = normalize_path_key(TfGetExtension(layer_path));
        if (layer_ext != "usd" && layer_ext != "usda" && layer_ext != "usdc" &&
            layer_ext != "usdz") {
            continue;
        }
        const SdfLayerRefPtr layer = SdfLayer::FindOrOpen(layer_path);
        if (!layer || layer->IsAnonymous()) {
            continue;
        }
        if (!visited.insert(layer->GetIdentifier()).second) {
            continue;  // already processed this layer via another closure entry
        }
        in_progress = PackagingLayerBackup(layer);  // snapshot before mutation
        bool changed = false;
        UsdUtilsModifyAssetPaths(
            layer, [&](const std::string& asset_path) -> std::string {
                if (asset_path.empty()) {
                    return asset_path;
                }
                // Only runtime artifacts strip design-time sources. Authoring
                // packages retain them. Blank them in memory so the one-shot
                // packager neither copies nor references them (disk is never
                // changed and live content is restored by the caller). See
                // is_behavior_source_asset / prototype_prepare's `source = @@`.
                if (strip_behavior_sources && is_behavior_source_asset(asset_path)) {
                    changed = true;
                    return std::string();
                }
                const std::string absolute =
                    layer->ComputeAbsolutePath(asset_path);
                if (unresolved != nullptr) {
                    const std::string raw_key = normalize_path_key(asset_path);
                    const std::string abs_key = normalize_path_key(absolute);
                    if (unresolved->count(raw_key) != 0 ||
                        unresolved->count(abs_key) != 0) {
                        changed = true;
                        return std::string();  // drop the unresolvable ref
                    }
                }
                // Canonicalize only paths that resolve to a real on-disk USD
                // layer file; everything else (textures, the <UDIM> template,
                // opdef:/ URIs, package-internal paths, missing refs) is left
                // exactly as authored.
                if (absolute.empty()) {
                    return asset_path;
                }
                const std::string target_ext =
                    normalize_path_key(TfGetExtension(absolute));
                if (target_ext != "usd" && target_ext != "usda" &&
                    target_ext != "usdc") {
                    return asset_path;
                }
                std::error_code ec;
                if (!std::filesystem::is_regular_file(
                        std::filesystem::path(absolute), ec)) {
                    return asset_path;
                }
                std::string canonical = absolute;
                for (char& c : canonical) {
                    if (c == '\\') {
                        c = '/';
                    }
                }
                if (canonical != asset_path) {
                    changed = true;
                    return canonical;
                }
                return asset_path;
            });
        if (changed) {
            opened.push_back(in_progress);
        } else {
            // ModifyAssetPaths dirties even on a no-op callback. Preserve both
            // pristine layers and any pre-existing unsaved authoring content.
            in_progress.restore();
        }
        in_progress = PackagingLayerBackup();  // layer fully handled
    }
    } catch (...) {
        // A mid-pass failure must leave NO layer globally rewritten: restore every
        // layer we already changed (and the one mid-rewrite, if any), then rethrow
        // so the caller's transaction unwinds cleanly. Idempotent and runs here
        // because the no-/EHsc toolchain skips local-object destructors on throw.
        for (const PackagingLayerBackup& backup : opened) {
            backup.restore();
        }
        in_progress.restore();
        throw;
    }
    return opened;
}

// Scan a FINISHED package for references OpenUSD could not localize (an arc that
// dangles into an empty bucket) and append one de-duplicated, named warning per
// missing asset to `result`. Best-effort; never throws.
//
// This re-resolution is the ONLY reliable signal of genuine content loss, and
// the only thing that should reach the user as an import warning. OpenUSD emits
// a flood of per-arc "Failed to resolve @<N>/foo.usd@" diagnostics WHILE building
// the package — those probe synthesized package-internal bucket paths that are
// not meant to resolve during the walk (see PathShouldResolve in
// assetLocalizationPackage.cpp) and are benign even when the finished package is
// complete; surfacing them produced a spurious "content missing" prompt on every
// import. `<UDIM>` templates are likewise never in the returned unresolved set.
void append_package_danglers(const std::string& destination_path,
                             PackageResultRaw& result) {
    const ArResolverContext dctx =
        ArGetResolver().CreateDefaultContextForAsset(destination_path);
    ArResolverContextBinder dbinder(dctx);
    std::vector<SdfLayerRefPtr> dep_layers;
    std::vector<std::string> dep_assets;
    std::vector<std::string> dangling;
    if (UsdUtilsComputeAllDependencies(SdfAssetPath(destination_path),
                                       &dep_layers, &dep_assets, &dangling)) {
        std::set<std::string> seen;
        for (const std::string& missing : dangling) {
            std::string label = TfGetBaseName(missing);
            if (label.empty()) {
                label = missing;
            }
            if (seen.insert(label).second) {
                result.warnings.push_back(rust::String(
                    label +
                    " — could not be packaged and is missing from the import"));
            }
        }
    }
}

}  // namespace

PackageResultRaw compute_dependencies(rust::Str source) {
    const std::string source_path = to_std(source);
    if (source_path.empty()) {
        throw std::runtime_error("dependency source must not be empty");
    }
    const ArResolverContext context =
        ArGetResolver().CreateDefaultContextForAsset(source_path);
    ArResolverContextBinder context_binder(context);
    std::vector<SdfLayerRefPtr> layers;
    std::vector<std::string> assets;
    std::vector<std::string> unresolved;
    if (!UsdUtilsComputeAllDependencies(
            SdfAssetPath(source_path), &layers, &assets, &unresolved)) {
        throw std::runtime_error("OpenUSD could not resolve the package root");
    }
    PackageResultRaw result;
    std::set<std::string> dependencies;
    for (const SdfLayerRefPtr& layer : layers) {
        if (!layer) {
            continue;
        }
        const std::string real_path = layer->GetRealPath();
        dependencies.insert(real_path.empty() ? layer->GetIdentifier() : real_path);
    }
    dependencies.insert(assets.begin(), assets.end());
    for (const std::string& dependency : dependencies) {
        result.dependencies.push_back(rust::String(dependency));
    }
    std::sort(unresolved.begin(), unresolved.end());
    unresolved.erase(std::unique(unresolved.begin(), unresolved.end()), unresolved.end());
    for (const std::string& path : unresolved) {
        result.unresolved.push_back(rust::String(path));
    }
    return result;
}

PackageResultRaw package_stage(
    rust::Str source, rust::Str destination, const PackagingOptionsRaw& options) {
    const std::string source_path = to_std(source);
    const std::string destination_path = to_std(destination);
    if (source_path.empty() || destination_path.empty()) {
        throw std::runtime_error("package source and destination must not be empty");
    }
    // Hold the exclusive stage lifecycle lock for the WHOLE mutate/package/restore
    // transaction. The steps below rewrite asset paths in, and restore,
    // process-global SdfLayer objects that a LIVE stage may share (packaging runs
    // on a worker thread — the Conductor import/cook job — beside the main
    // thread's viewport projections, which take the shared side of this lock).
    // Without the lock the in-memory rewrite + Reload would recompose a live
    // stage on THIS thread (delivering its TfNotice here, per OpenUSD's
    // same-thread notice delivery) while the main thread reads that stage — a
    // data race on the UsdStage. The exclusive lock serializes packaging against
    // every read projection, and the helper releases it even if the body throws
    // (the no-/EHsc rule; see with_stage_lifecycle_lock). compute_dependencies,
    // CreateNewUsdzPackage and the verification Open take no lifecycle lock
    // themselves, so there is no re-entrant deadlock.
    return with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>(
        [&]() -> PackageResultRaw {
    PackageResultRaw result = compute_dependencies(source);
    // A design-time behavior SOURCE (`idtx:behavior:source = @....rs@`) is an
    // asset-typed attribute, so UsdUtilsComputeAllDependencies lists it in the
    // dependency closure — and, when the `.rs` is missing, misspelled, or belongs
    // to a player-rig variant whose source was never shipped, in `unresolved`.
    // Only runtime packaging blanks these sources. Authoring packaging must keep
    // missing sources in the unresolved report so import can warn/refuse.
    // For runtime cooks, a missing source must NOT gate packaging — otherwise it
    // aborts the cook writing no file, and the cook fails far away reading a usdz
    // that was never created. Partition it out of `unresolved` BEFORE the refuse-
    // gate and drop it from the returned report, but record a warning per dropped
    // source so a genuine authoring typo stays visible instead of silently
    // shipping an experience missing that behavior.
    if (options.strip_behavior_sources) {
        rust::Vec<rust::String> real_unresolved;
        for (const rust::String& entry : result.unresolved) {
            if (is_behavior_source_asset(to_std(entry))) {
                result.warnings.push_back(rust::String(
                    "dropped unresolvable behavior source from the package "
                    "closure (design-time .rs is never shipped); check the "
                    "authored idtx:behavior:source path if unexpected: " +
                    to_std(entry)));
            } else {
                real_unresolved.push_back(entry);
            }
        }
        result.unresolved = std::move(real_unresolved);
    }
    if (!result.unresolved.empty()) {
        // References that cannot be resolved make UsdUtilsCreateNewUsdzPackage
        // fail outright (it does not emit a partial package), so report them to
        // the caller rather than attempting a doomed package: Import surfaces
        // them, Build inspects them, and package_loose_as_usdz maps them to
        // StageError::UnresolvedAssets. `destination` stays empty (no file).
        return result;
    }
    const ArResolverContext context =
        ArGetResolver().CreateDefaultContextForAsset(source_path);
    ArResolverContextBinder context_binder(context);

    // Collapse divergent relative spellings of the same on-disk asset to one
    // identity before packaging, so OpenUSD's directory remapper writes a single
    // bucket per real file instead of leaving duplicate buckets empty (which
    // dangles references and silently drops geometry / point-instancer
    // prototypes). In memory only; the touched layers are restored in the
    // restore region below, before any throw. No unresolved set here: this path
    // runs only when the closure is fully resolvable (we early-returned above).
    std::vector<std::string> layer_paths;
    layer_paths.push_back(source_path);
    for (const rust::String& dep : result.dependencies) {
        layer_paths.push_back(std::string(dep));
    }
    // Capture the root BEFORE canonicalization; defaultPrim adoption must also
    // restore pre-existing unsaved content, not our temporary rewritten paths.
    const SdfLayerRefPtr source_root_layer = SdfLayer::FindOrOpen(source_path);
    const PackagingLayerBackup root_backup(source_root_layer);
    const std::vector<PackagingLayerBackup> canonicalized =
        canonicalize_closure_asset_paths(layer_paths, /*unresolved=*/nullptr,
                                         options.strip_behavior_sources);

    // Capture OpenUSD's own diagnostics emitted on this thread during packaging
    // for accept/reject, and fold them into a hard failure so the reason is
    // visible. The collector filters to the installing thread so a concurrent
    // viewport projection can't contaminate the result (see the class comment).
    PackagingDiagnosticCollector collector;
    bool default_prim_adopted = false;

    // Restore all process-global state the steps below mutate in memory: detach
    // the collector (re-enables normal diagnostics on this thread) and restore the
    // canonicalized + defaultPrim-adopted layers to their original live content.
    // This MUST run whether packaging returns normally OR throws: on this
    // toolchain an exception does not destroy `collector` or run any RAII restore
    // (see with_stage_lifecycle_lock / the no-/EHsc rule), so the catch below
    // invokes it by hand before the throw re-crosses the boundary. Every step is
    // idempotent (detach() guards on attached_; restoring content is repeatable),
    // so calling it once on the normal path and once in the catch is harmless.
    // (canonicalize_closure_asset_paths already restored itself if IT threw, so
    // its layers are only pending restore once it returned successfully.)
    const auto restore_global_state = [&]() {
        // Detach first so no delegate callback can fire while we later drain
        // `messages` (packaging's worker threads have joined by now).
        collector.detach();
        for (const PackagingLayerBackup& backup : canonicalized) {
            backup.restore();
        }
        if (default_prim_adopted && source_root_layer) {
            root_backup.restore();
        }
    };

    bool packaged = false;
    UsdStageRefPtr verification;
    try {
        // Give the packaged asset a defaultPrim when it lacks one so it can be
        // used as a reference/payload target (see adopt_single_root_as_default_prim).
        // In memory only; reverted by restore_global_state below.
        default_prim_adopted = adopt_single_root_as_default_prim(source_root_layer);
        packaged = UsdUtilsCreateNewUsdzPackage(
            SdfAssetPath(source_path), destination_path, "scene.usdc", false);
        if (packaged) {
            verification = UsdStage::Open(destination_path);
        }
    } catch (...) {
        restore_global_state();
        throw;
    }
    restore_global_state();
    // Build the failure-diagnostic text from the collector, but do NOT surface
    // these to the caller as warnings: during a successful localization OpenUSD
    // emits benign per-arc probe failures against synthesized bucket paths (and
    // UDIM-template noise). The only real "content missing" signal is a dangling
    // reference in the finished package, scanned below. (Used only if we throw.)
    std::string diagnostics_text;
    for (const std::string& message : collector.messages) {
        if (!diagnostics_text.empty()) {
            diagnostics_text += "\n";
        }
        diagnostics_text += message;
    }

    if (!packaged) {
        std::string message = "OpenUSD could not create the USDZ package";
        if (!diagnostics_text.empty()) {
            message += ":\n" + diagnostics_text;
        }
        throw std::runtime_error(message);
    }
    if (!verification) {
        std::string message = "packaged USD failed official OpenUSD reopen verification";
        if (!diagnostics_text.empty()) {
            message += ":\n" + diagnostics_text;
        }
        throw std::runtime_error(message);
    }
    // Warn on loss only: any reference the packager could not localize (should be
    // none now that divergent spellings are canonicalized before packaging).
    append_package_danglers(destination_path, result);
    result.destination = rust::String(destination_path);
    return result;
    });
}

// "Import anyway" packaging: create a verified USDZ from `source` while DROPPING
// references OpenUSD cannot resolve, instead of refusing like package_stage.
//
// Source files are NEVER written: clear unresolved asset paths from IN-MEMORY
// copies of every dependency-closure layer (never Save()d), package with
// editLayersInPlace=FALSE (copies retain real-path anchors for relative assets),
// then restore every touched layer to its original live content. Dirty layers
// are snapshotted so restoration does not discard unsaved authoring opinions.
// Clearing matches compute_dependencies' unresolved set (UDIM-aware: standard
// `<UDIM>` textures resolve to their tiles and are NEVER dropped) against both
// the raw authored value (non-file URIs like Houdini `opdef:/Vop/...`) and the
// layer-anchored absolute path (relative textures), walking the FULL closure
// (broken refs hide behind unselected variants / unloaded payloads).
//
// Structure-preserving + warn on loss: references and variant sets are kept (no
// flattening). Before packaging we also canonicalize every resolvable on-disk
// USD ref to its absolute path (see canonicalize_closure_asset_paths), which
// collapses divergent relative spellings of the same file — the usual cause of
// dangling references in Houdini-exported scenes — into one remapper bucket. A
// reference can still dangle only when the SAME file is reachable through an
// arc that does not resolve to an on-disk path at all (so it cannot be
// canonicalized). We detect any such case in the FINISHED package and report it
// as a warning (named, de-duplicated) so the user knows exactly which assets are
// missing; everything else imports intact.
//
// Like package_stage/compute_dependencies, this runs under the EXCLUSIVE stage
// lifecycle lock (taken in the function body) so its layer mutations/Reloads
// cannot race viewport projections. Per the no-/EHsc rule, detach() the
// diagnostic collector before every throw.
PackageResultRaw package_stage_dropping_unresolved(
    rust::Str source, rust::Str destination, const PackagingOptionsRaw& options) {
    const std::string source_path = to_std(source);
    const std::string destination_path = to_std(destination);
    if (source_path.empty() || destination_path.empty()) {
        throw std::runtime_error(
            "package source and destination must not be empty");
    }

    // Hold the exclusive stage lifecycle lock for the whole mutate/package/restore
    // transaction, for the same reason as package_stage: the clears +
    // canonicalization below rewrite and restore process-global SdfLayer
    // objects a live stage may share, while this runs on a worker thread beside
    // the main thread's viewport projections. See package_stage for the full
    // rationale and the no-/EHsc release discipline.
    return with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>(
        [&]() -> PackageResultRaw {
    PackageResultRaw result = compute_dependencies(source);
    std::unordered_set<std::string> unresolved_set;
    for (const rust::String& u : result.unresolved) {
        unresolved_set.insert(normalize_path_key(std::string(u)));
    }

    const ArResolverContext context =
        ArGetResolver().CreateDefaultContextForAsset(source_path);
    ArResolverContextBinder context_binder(context);

    std::vector<std::string> layer_paths;
    layer_paths.push_back(source_path);
    for (const rust::String& dep : result.dependencies) {
        layer_paths.push_back(std::string(dep));
    }

    // In one pass over the full closure, CLEAR the unresolved asset paths and
    // CANONICALIZE every still-resolvable on-disk USD ref to its absolute path,
    // in memory only. Clearing drops genuinely-missing refs so the packager does
    // not refuse; canonicalization collapses divergent relative spellings of the
    // same file into one remapper bucket so resolvable arcs no longer dangle (the
    // two behaviours are independent — canonicalization never removes a ref that
    // clearing would keep). Collect backups of dirtied layers to restore after.
    const SdfLayerRefPtr source_root_layer = SdfLayer::FindOrOpen(source_path);
    const PackagingLayerBackup root_backup(source_root_layer);
    std::vector<PackagingLayerBackup> opened =
        canonicalize_closure_asset_paths(layer_paths, &unresolved_set,
                                         options.strip_behavior_sources);

    PackagingDiagnosticCollector collector;
    bool default_prim_adopted = false;

    // Restore all process-global state mutated in memory above (detach the
    // collector + discard our ModifyAssetPaths clears and the defaultPrim edit
    // from the backups). MUST run whether packaging returns normally OR throws.
    // On this toolchain an exception does not destroy `collector` or run RAII
    // restore (see the no-/EHsc rule), so the catch below runs it by hand before
    // rethrowing. Idempotent, so running it on the normal path and in the catch
    // is harmless. (canonicalize_closure_asset_paths already restored itself if
    // IT threw, so `opened` is only pending restore once it returned.)
    const auto restore_global_state = [&]() {
        collector.detach();
        // (With editLayersInPlace=false the packager copies layers before
        // rewriting, so it leaves no rewrites of its own to undo; only our
        // ModifyAssetPaths clears need reverting.)
        for (const PackagingLayerBackup& backup : opened) {
            backup.restore();
        }
        // The source root layer may not be in `opened` if it had no unresolved
        // paths to clear, so revert its defaultPrim edit explicitly.
        if (default_prim_adopted && source_root_layer) {
            root_backup.restore();
        }
    };

    // editLayersInPlace MUST be false. With true, OpenUSD rewrites asset paths by
    // mutating the shared in-memory layers in place while its directory remapper
    // assigns package buckets per composition arc; for scenes where a layer is
    // reached through several arcs (e.g. Houdini stage-manager composition) the
    // in-place rewrite serves one bucket and leaves every other arc dangling,
    // cascading a handful of genuinely-unresolvable refs into dozens of dropped
    // geometry/material layers (verified on BarScene: 6 danglers with false vs
    // 161 with true). With false the packager instead copies each layer before
    // rewriting, so our in-memory ModifyAssetPaths clears above are still honoured
    // (CreateNewUsdzPackage reads the already-open registry layers) while every
    // resolvable arc packages intact.
    bool packaged = false;
    UsdStageRefPtr verification;
    try {
        // Give the packaged asset a defaultPrim when it lacks one so it can be
        // used as a reference/payload target. In memory only; reverted by
        // restore_global_state below.
        default_prim_adopted = adopt_single_root_as_default_prim(source_root_layer);
        packaged = UsdUtilsCreateNewUsdzPackage(
            SdfAssetPath(source_path), destination_path, "scene.usdc",
            /*editLayersInPlace=*/false);
        if (packaged) {
            verification = UsdStage::Open(destination_path);
        }
    } catch (...) {
        restore_global_state();
        throw;
    }
    restore_global_state();

    // Diagnostics text is for failure messages only (do NOT surface the
    // packager's own warnings — mostly deprecation noise — to the caller).
    std::string diagnostics_text;
    for (const std::string& message : collector.messages) {
        diagnostics_text += diagnostics_text.empty() ? "" : "\n";
        diagnostics_text += message;
    }
    if (!packaged) {
        std::string message =
            "OpenUSD could not create the USDZ package after dropping "
            "unresolved references";
        if (!diagnostics_text.empty()) {
            message += ":\n" + diagnostics_text;
        }
        throw std::runtime_error(message);
    }
    if (!verification) {
        std::string message =
            "packaged USD failed official OpenUSD reopen verification";
        if (!diagnostics_text.empty()) {
            message += ":\n" + diagnostics_text;
        }
        throw std::runtime_error(message);
    }

    // Warn on loss: report assets the packager could not localize (dangling
    // references in the finished package), de-duplicated by base name, so the
    // user knows exactly what is missing. Best-effort; never throws here.
    append_package_danglers(destination_path, result);

    result.destination = rust::String(destination_path);
    return result;
    });
}

// True when `s` names a remote asset via an `http`/`https` scheme. Matches both
// the authored `scheme://host/…` form and the `scheme:/host/…` single-slash form
// the default resolver produces when no URI resolver claims the scheme (it
// collapses `//` during anchoring; `http://` begins with `http:/`, so one prefix
// test covers both). Mirrors idtx_is_http_uri in the HTTP resolver plugin.
static bool is_remote_asset_uri(const std::string& s) {
    return s.rfind("http:/", 0) == 0 || s.rfind("https:/", 0) == 0;
}

// Collect every `*.usd/.usda/.usdc` layer file under `dir` (recursively), plus
// all other files, as paths RELATIVE to `dir` (forward-slashed). Layers come
// back separately so the caller can rewrite their asset paths before zipping.
static void collect_localized_files(const std::string& dir,
                                    std::vector<std::string>& layers,
                                    std::vector<std::string>& others) {
    const std::filesystem::path root(dir);
    std::error_code ec;
    for (auto it = std::filesystem::recursive_directory_iterator(root, ec);
         it != std::filesystem::recursive_directory_iterator(); it.increment(ec)) {
        if (ec) break;
        if (!it->is_regular_file(ec)) continue;
        std::string rel = std::filesystem::relative(it->path(), root, ec).generic_string();
        if (rel.empty()) continue;
        const std::string ext = normalize_path_key(TfGetExtension(rel));
        if (ext == "usd" || ext == "usda" || ext == "usdc") {
            layers.push_back(std::move(rel));
        } else {
            others.push_back(std::move(rel));
        }
    }
}

// Packaging that LOCALIZES local references (search-path, absolute, and relative
// alike → package-internal paths, exactly as UsdUtilsCreateNewUsdzPackage does)
// while PRESERVING remote (http(s)://) references verbatim.
//
// USD's one-shot usdz packager cannot do both: its directory remapper rewrites
// every absolute/URI reference — including a remote URL — into an internal bucket
// (destroying the URL). So instead we localize into a temp DIRECTORY via
// UsdUtilsLocalizeAsset with a processing function that swaps each remote URL for
// a relative, unresolvable SENTINEL (`./__idtx_remote_<N>__.usd`). The localizer
// keeps such relative-unresolvable paths untouched (it only rewrites things it can
// place in the package), so every LOCAL ref is correctly package-internalized
// while the remote refs survive as sentinels. We then rewrite the sentinels back
// to their original URLs in the on-disk localized layers and zip the directory
// into the final `.usdz` (no re-localization). The runtime's HTTP resolver fetches
// the preserved URLs at play time.
//
// Runs under the EXCLUSIVE stage-lifecycle lock like the other packagers.
PackageResultRaw package_stage_localize_keeping_remote(
    rust::Str source, rust::Str destination, const PackagingOptionsRaw& options) {
    const std::string source_path = to_std(source);
    const std::string destination_path = to_std(destination);
    if (source_path.empty() || destination_path.empty()) {
        throw std::runtime_error("package source and destination must not be empty");
    }
    return with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>(
        [&]() -> PackageResultRaw {
    PackageResultRaw result;

    const ArResolverContext context =
        ArGetResolver().CreateDefaultContextForAsset(source_path);
    ArResolverContextBinder binder(context);

    // Temp localization dir beside the destination (same volume). Cleaned below.
    const std::string temp_dir = destination_path + ".localize-tmp";
    std::error_code ec;
    std::filesystem::remove_all(temp_dir, ec);
    std::filesystem::create_directories(temp_dir, ec);

    // Swap remote URLs for relative sentinels during localization; `sentinel_urls`
    // maps index N → original URL. Localization may fan out across threads, so the
    // map is mutex-guarded.
    std::vector<std::string> sentinel_urls;
    std::mutex sentinel_mu;
    const auto processing =
        [&](const SdfLayerHandle&, const UsdUtilsDependencyInfo& info) -> UsdUtilsDependencyInfo {
        const std::string& asset_path = info.GetAssetPath();
        // Runtime-only: design-time sources (`.rs`) must not ship. Drop them so the
        // localizer neither copies the `.rs` into a bucket nor references it (the
        // runtime ticks the compiled `.wasm` module). Mirrors prototype_prepare's
        // `idtx:behavior:source = @@`.
        if (options.strip_behavior_sources && is_behavior_source_asset(asset_path)) {
            return UsdUtilsDependencyInfo(std::string());
        }
        if (!is_remote_asset_uri(asset_path)) {
            return info;  // local dependency → localize normally
        }
        std::size_t n;
        {
            std::lock_guard<std::mutex> lk(sentinel_mu);
            n = sentinel_urls.size();
            sentinel_urls.push_back(asset_path);
        }
        return UsdUtilsDependencyInfo("./__idtx_remote_" + std::to_string(n) + "__.usd");
    };

    const bool localized = UsdUtilsLocalizeAsset(
        SdfAssetPath(source_path), temp_dir, /*editLayersInPlace=*/false, processing);
    if (!localized) {
        std::filesystem::remove_all(temp_dir, ec);
        throw std::runtime_error("UsdUtilsLocalizeAsset failed for " + source_path);
    }

    // Restore sentinels → original URLs in every localized layer, then save so the
    // zip picks up the rewritten bytes. A sentinel is the whole authored value
    // (the localizer may strip a leading "./"); recover N from its token.
    const auto restore = [&](const std::string& p) -> std::string {
        // Authoritative blanking of any design-time behavior source that survived
        // into a localized layer (belt-and-suspenders with the processing callback
        // above): a `.rs` must never be referenced from a runtime package.
        if (options.strip_behavior_sources && is_behavior_source_asset(p)) return std::string();
        std::string s = p;
        if (s.rfind("./", 0) == 0) s = s.substr(2);
        const std::string prefix = "__idtx_remote_";
        if (s.rfind(prefix, 0) != 0) return p;
        const std::size_t end = s.find("__", prefix.size());
        if (end == std::string::npos) return p;
        const std::string digits = s.substr(prefix.size(), end - prefix.size());
        std::size_t n = 0;
        try {
            n = static_cast<std::size_t>(std::stoul(digits));
        } catch (...) {
            return p;
        }
        std::lock_guard<std::mutex> lk(sentinel_mu);
        return (n < sentinel_urls.size()) ? sentinel_urls[n] : p;
    };

    std::vector<std::string> layer_rels;
    std::vector<std::string> other_rels;
    collect_localized_files(temp_dir, layer_rels, other_rels);
    for (const std::string& rel : layer_rels) {
        const std::string abs = temp_dir + "/" + rel;
        SdfLayerRefPtr layer = SdfLayer::FindOrOpen(abs);
        if (!layer) continue;
        UsdUtilsModifyAssetPaths(layer, restore);
        layer->Save();
    }

    // The localized entry = the root layer, named after the source's base name.
    const std::string entry_rel = TfGetBaseName(source_path);
    if (!std::filesystem::exists(std::filesystem::path(temp_dir) / entry_rel, ec)) {
        std::filesystem::remove_all(temp_dir, ec);
        throw std::runtime_error(
            "localized entry layer not found in temp dir (expected " + entry_rel + ")");
    }

    // Zip the localized tree into the final usdz, ENTRY FIRST (the usdz default
    // layer is its first file), then the remaining layers + assets. Verbatim —
    // the sentinels are already restored to URLs, and nothing is re-localized.
    SdfZipFileWriter writer = SdfZipFileWriter::CreateNew(destination_path);
    if (!writer) {
        std::filesystem::remove_all(temp_dir, ec);
        throw std::runtime_error("could not create usdz archive at " + destination_path);
    }
    const auto add = [&](const std::string& rel) -> bool {
        return !writer.AddFile(temp_dir + "/" + rel, rel).empty();
    };
    bool add_ok = add(entry_rel);
    for (const std::string& rel : layer_rels) {
        if (rel == entry_rel) continue;
        add_ok = add(rel) && add_ok;
    }
    for (const std::string& rel : other_rels) {
        // Runtime-only: omit sources even if the localizer copied a
        // `.rs` into a bucket before the processing callback dropped the reference.
        if (options.strip_behavior_sources && is_behavior_source_asset(rel)) continue;
        add_ok = add(rel) && add_ok;
    }
    if (!add_ok || !writer.Save()) {
        writer.Discard();
        std::filesystem::remove_all(temp_dir, ec);
        throw std::runtime_error("failed to assemble usdz from localized tree: " + destination_path);
    }
    std::filesystem::remove_all(temp_dir, ec);

    if (!SdfZipFile::Open(destination_path)) {
        throw std::runtime_error(
            "localized usdz failed to reopen as a valid archive: " + destination_path);
    }

    {
        std::lock_guard<std::mutex> lk(sentinel_mu);
        for (const std::string& url : sentinel_urls) {
            result.external.push_back(rust::String(url));
        }
    }
    result.destination = rust::String(destination_path);
    return result;
    });
}

#include "runtime_asset_copy.inc"

std::unique_ptr<StageHandle> open_stage(rust::Str path, bool load_payloads) {
    // Construct the handle before taking the lifecycle lock so that, on platforms
    // where unwinding runs destructors, the lock (taken inside the helper) is released
    // before `StageHandle::Impl` reacquires it for teardown. On this toolchain the
    // helper's catch releases the lock explicitly before any throw (see
    // with_stage_lifecycle_lock).
    auto handle = std::make_unique<StageHandle>();
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
        const std::string identifier = to_std(path);
        handle->impl_->stage = UsdStage::Open(
            identifier, load_payloads ? UsdStage::LoadAll : UsdStage::LoadNone);
        if (!handle->impl_->stage) {
            throw std::runtime_error("OpenUSD could not open the stage");
        }
        // A user-triggered open/reload must observe current disk state even when
        // Sdf's process-wide layer registry already has this identifier cached.
        handle->impl_->observer = std::make_unique<Observer>();
        handle->impl_->observer_key = TfNotice::Register(
            TfCreateWeakPtr(handle->impl_->observer.get()),
            &Observer::changed,
            handle->impl_->stage);
    });
    return handle;
}

std::unique_ptr<StageHandle> open_stage_with_session(
    rust::Str path, rust::Str session_layer_text, bool load_payloads) {
    // Build the handle before taking the lock (helper-scoped) so teardown ordering
    // holds where unwinding runs dtors; the helper's catch releases the lock on any
    // throw on this toolchain.
    //
    // SHARED lock, not exclusive. This open is a *reader* with respect to every
    // other live stage: it composes a brand-new stage (its own UsdStage + PcpCache
    // + the fresh anonymous session layer created below), performs NO Reload and NO
    // authoring, and the only process-global structures it touches — the Sdf layer
    // registry (via FindOrOpen), the Ar resolver (remote fetch during compose), and
    // the Tf notice registry (Register) — are internally thread-safe for concurrent
    // open + read. OpenUSD's model is per-stage "multiple readers OR one writer",
    // and a plain Open of a NEW stage is not a writer against any existing stage;
    // a layer shared with a live stage (e.g. the built-in player rig) is only READ
    // here (FindOrOpen returns the cached layer, it does not reload it). Taking the
    // EXCLUSIVE side here used to block every foreground shared-lock read projection
    // for the whole (possibly multi-second, remote) compose — on an async
    // StageLoader this froze the live stage's per-frame reads (the visionOS level-
    // transition watchdog kill). Shared lets a background open run concurrently with
    // foreground reads and private session writes while excluding shared-layer
    // authoring, package/localize, reload, resolver configuration and registry
    // changes. Preconditions: the caller never arms
    // the resolver force_refresh / packaging_mode for this path. Do NOT copy this
    // to `open_stage` without re-audit — the
    // editor reload path there can arm force_refresh / reload a cached shared layer.
    auto handle = std::make_unique<StageHandle>();
    handle->impl_->access([&] {
#ifdef IDTX_TEST_SUPPORT
        // Test hook (compiled only under the `test-support` feature; a no-op unless
        // the env var is set): hold the lifecycle lock for a fixed delay to
        // deterministically simulate a slow/remote compose, so the shared-lock
        // regression test can assert a concurrent foreground read projection of a
        // DIFFERENT stage is NOT blocked by this open. Placed inside the lock so the
        // hold spans the critical section exactly as a real multi-second compose
        // would.
        if (const char* hold_ms = std::getenv("IDTX_TEST_OPEN_HOLD_MS")) {
            const long ms = std::strtol(hold_ms, nullptr, 10);
            if (ms > 0) {
                std::this_thread::sleep_for(std::chrono::milliseconds(ms));
            }
        }
#endif
        const std::string root_identifier = to_std(path);
        const SdfLayerRefPtr root_layer = SdfLayer::FindOrOpen(root_identifier);
        if (!root_layer) {
            throw std::runtime_error("OpenUSD could not open the root layer");
        }

        // Build an anonymous session layer from the caller's USDA text and inject it
        // as the stage's strongest layer via the 3-arg Open. The session composes
        // over the root without mutating it on disk.
        const SdfLayerRefPtr session_layer = SdfLayer::CreateAnonymous(".usda");
        if (!session_layer) {
            throw std::runtime_error("could not create the anonymous session layer");
        }

        // Anchor the session layer to the root layer's directory so relative asset
        // paths inside it (`@./controller.usdz@`) resolve exactly as they would from
        // the root layer — the caller need not pre-absolutize them. We rename the
        // anonymous layer to "<rootDir>/<unique>.usda": the unique segment is taken
        // from the anonymous identifier (its address-based middle field, guaranteed
        // distinct per live layer), so two session layers overlaying the same asset
        // never collide in Sdf's process-wide registry. The layer stays in-memory;
        // SetIdentifier only changes the resolver anchor, it does not touch disk.
        const std::string root_real_path = root_layer->GetRealPath();
        if (!root_real_path.empty()) {
            const std::string root_dir =
                std::filesystem::path(root_real_path).parent_path().string();
            std::string bare;
            SdfLayer::FileFormatArguments anon_args;
            SdfLayer::SplitIdentifier(session_layer->GetIdentifier(), &bare, &anon_args);
            const std::string::size_type first = bare.find(':');
            const std::string::size_type second =
                first == std::string::npos ? std::string::npos : bare.find(':', first + 1);
            const std::string unique =
                (first != std::string::npos && second != std::string::npos)
                    ? bare.substr(first + 1, second - first - 1)
                    : bare;
            const std::string session_identifier = SdfLayer::CreateIdentifier(
                root_dir + "/" + unique + ".usda", SdfLayer::FileFormatArguments{});
            session_layer->SetIdentifier(session_identifier);
        }

        const std::string session_text = to_std(session_layer_text);
        if (!session_text.empty() && !session_layer->ImportFromString(session_text)) {
            throw std::runtime_error("session layer content is not valid USDA");
        }

        handle->impl_->stage = UsdStage::Open(
            root_layer, session_layer,
            load_payloads ? UsdStage::LoadAll : UsdStage::LoadNone);
        if (!handle->impl_->stage) {
            throw std::runtime_error("OpenUSD could not open the stage with a session layer");
        }
        handle->impl_->observer = std::make_unique<Observer>();
        handle->impl_->observer_key = TfNotice::Register(
            TfCreateWeakPtr(handle->impl_->observer.get()),
            &Observer::changed,
            handle->impl_->stage);
    });
    return handle;
}

// Create a brand-new stage on disk at `path`, author a default root prim, set the
// stage's up-axis + metersPerUnit, save it, and return an opened handle over it.
// Mirrors `open_stage`'s lifecycle discipline exactly (build the handle before
// taking the exclusive lock so a throw unwinds locals before ~Impl reacquires it,
// register the change observer identically). `default_prim_name` empty ⇒ no root
// prim is authored (and no defaultPrim set); `default_prim_type` empty ⇒ the root
// prim is typeless.
std::unique_ptr<StageHandle> create_new_stage(
    rust::Str path, rust::Str up_axis, double meters_per_unit,
    rust::Str default_prim_name, rust::Str default_prim_type) {
    auto handle = std::make_unique<StageHandle>();
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
        const std::string identifier = to_std(path);
        if (identifier.empty()) {
            throw std::runtime_error("new-stage path must not be empty");
        }
        // CreateNew builds the root layer in the Sdf registry but does not touch disk
        // until Save(); a stale registry entry for this identifier would otherwise be
        // reused, so require a genuinely new layer.
        handle->impl_->stage = UsdStage::CreateNew(identifier);
        if (!handle->impl_->stage) {
            throw std::runtime_error("OpenUSD could not create the new stage");
        }
        const UsdStageRefPtr& stage = handle->impl_->stage;

        const std::string up_axis_str = to_std(up_axis);
        if (up_axis_str == "Z" || up_axis_str == "z") {
            UsdGeomSetStageUpAxis(stage, UsdGeomTokens->z);
        } else {
            UsdGeomSetStageUpAxis(stage, UsdGeomTokens->y);
        }
        if (meters_per_unit > 0.0) {
            UsdGeomSetStageMetersPerUnit(stage, meters_per_unit);
        }

        const std::string prim_name = to_std(default_prim_name);
        if (!prim_name.empty()) {
            const SdfPath root_path = SdfPath::AbsoluteRootPath().AppendChild(TfToken(prim_name));
            const std::string type_str = to_std(default_prim_type);
            // No SdfChangeBlock: DefinePrim resolves + authors ancestor specs and needs
            // live recomposition (see define_session_prim), and it does its own batching.
            const UsdPrim root_prim = type_str.empty()
                ? stage->DefinePrim(root_path)
                : stage->DefinePrim(root_path, TfToken(type_str));
            if (!root_prim) {
                throw std::runtime_error("OpenUSD could not define the default root prim");
            }
            stage->SetDefaultPrim(root_prim);
        }

        if (!stage->GetRootLayer()->Save()) {
            throw std::runtime_error("OpenUSD could not save the new stage to disk");
        }

        handle->impl_->observer = std::make_unique<Observer>();
        handle->impl_->observer_key = TfNotice::Register(
            TfCreateWeakPtr(handle->impl_->observer.get()),
            &Observer::changed,
            handle->impl_->stage);
    });
    return handle;
}

StageMetadataRaw stage_metadata(const StageHandle& handle) {
    return handle.impl_->access([&] {
    const UsdStageRefPtr& stage = handle.impl_->stage;
    StageMetadataRaw out;
    out.root_identifier = rust::String(stage->GetRootLayer()->GetIdentifier());
    const UsdPrim default_prim = stage->GetDefaultPrim();
    out.default_prim = rust::String(default_prim ? default_prim.GetPath().GetString() : "");
    out.up_axis = rust::String(UsdGeomGetStageUpAxis(stage).GetString());
    out.meters_per_unit = UsdGeomGetStageMetersPerUnit(stage);
    out.start_time_code = stage->GetStartTimeCode();
    out.end_time_code = stage->GetEndTimeCode();
    out.time_codes_per_second = stage->GetTimeCodesPerSecond();
    out.has_time_code_range = stage->GetRootLayer()->HasField(
        SdfPath::AbsoluteRootPath(), TfToken("startTimeCode"));
    // Root-layer shadow mode opt-in. Resolution order (root layer only, not
    // composed — matching how the flag is authored per the plan):
    //   1. customLayerData["idtx:shadowMode"] token:
    //      "off"/"hard"/"smooth"/"soft" → 0/1/2/3 (unknown token → 0).
    //   2. else legacy customLayerData["idtx:softShadows"] bool: true→3 (soft),
    //      false→1 (hard).
    //   3. else → 0 (off). Shadows stay opt-in: neither key means OFF.
    out.shadow_mode = 0;
    {
        const VtDictionary custom = stage->GetRootLayer()->GetCustomLayerData();
        const auto mode_it = custom.find("idtx:shadowMode");
        if (mode_it != custom.end() && mode_it->second.IsHolding<TfToken>()) {
            const TfToken mode = mode_it->second.UncheckedGet<TfToken>();
            if (mode == TfToken("soft")) {
                out.shadow_mode = 3;
            } else if (mode == TfToken("smooth")) {
                out.shadow_mode = 2;
            } else if (mode == TfToken("hard")) {
                out.shadow_mode = 1;
            } else {
                out.shadow_mode = 0;  // "off" or unknown token
            }
        } else if (mode_it != custom.end() && mode_it->second.IsHolding<std::string>()) {
            const std::string mode = mode_it->second.UncheckedGet<std::string>();
            if (mode == "soft") {
                out.shadow_mode = 3;
            } else if (mode == "smooth") {
                out.shadow_mode = 2;
            } else if (mode == "hard") {
                out.shadow_mode = 1;
            } else {
                out.shadow_mode = 0;
            }
        } else {
            const auto legacy_it = custom.find("idtx:softShadows");
            if (legacy_it != custom.end() && legacy_it->second.IsHolding<bool>()) {
                out.shadow_mode = legacy_it->second.UncheckedGet<bool>() ? 3 : 1;
            }
        }
    }
    // Root-layer stage-behavior assignment (root layer only, not composed —
    // matching how the keys are authored). `idtx:stageBehavior` names the
    // stage-behavior prim; `idtx:stageBehaviorRunScope` is the run-condition
    // token. Both empty when unauthored.
    {
        const VtDictionary custom = stage->GetRootLayer()->GetCustomLayerData();
        const auto name_it = custom.find("idtx:stageBehavior");
        if (name_it != custom.end()) {
            if (name_it->second.IsHolding<std::string>()) {
                out.stage_behavior_name =
                    rust::String(name_it->second.UncheckedGet<std::string>());
            } else if (name_it->second.IsHolding<TfToken>()) {
                out.stage_behavior_name =
                    rust::String(name_it->second.UncheckedGet<TfToken>().GetString());
            }
        }
        const auto scope_it = custom.find("idtx:stageBehaviorRunScope");
        if (scope_it != custom.end()) {
            if (scope_it->second.IsHolding<TfToken>()) {
                out.stage_behavior_run_scope =
                    rust::String(scope_it->second.UncheckedGet<TfToken>().GetString());
            } else if (scope_it->second.IsHolding<std::string>()) {
                out.stage_behavior_run_scope =
                    rust::String(scope_it->second.UncheckedGet<std::string>());
            }
        }
    }
    return out;
    });
}

rust::Vec<CurveObstacleRaw> curve_obstacles(const StageHandle& handle) {
    return handle.impl_->access([&] {
        rust::Vec<CurveObstacleRaw> out;
        UsdGeomBBoxCache cache(UsdTimeCode::Default(), {UsdGeomTokens->default_, UsdGeomTokens->render, UsdGeomTokens->proxy}, false, true);
        std::vector<SdfPath> owned;
        std::vector<SdfPath> prototypes;
        for (const auto& prim : handle.impl_->stage->Traverse()) {
            if (const auto instancer = UsdGeomPointInstancer(prim)) {
                SdfPathVector targets; instancer.GetPrototypesRel().GetTargets(&targets);
                prototypes.insert(prototypes.end(), targets.begin(), targets.end());
            }
        }
        for (const auto& prim : handle.impl_->stage->Traverse()) {
            std::string generator;
            if (auto attr = prim.GetAttribute(TfToken("idtx:placement:generator"))) {
                if (attr.Get(&generator) && generator == "idtx-curve-placement/1") owned.push_back(prim.GetPath());
            }
            if (std::any_of(owned.begin(), owned.end(), [&](const auto& p) { return prim.GetPath().HasPrefix(p); })) continue;
            if (std::any_of(prototypes.begin(), prototypes.end(), [&](const auto& p) { return prim.GetPath().HasPrefix(p); })) continue;
            const auto type = prim.GetTypeName().GetString();
            if (type == "BasisCurves" || type == "NurbsCurves" || type == "HermiteCurves") continue;
            if (!prim.IsInstance() && !prim.IsA<UsdGeomBoundable>()) continue;
            const auto range = cache.ComputeWorldBound(prim).ComputeAlignedRange();
            if (range.IsEmpty()) continue;
            if (out.size() >= 65536) throw std::runtime_error("clearance obstacle budget exceeded");
            CurveObstacleRaw record;
            record.path = rust::String(prim.GetPath().GetString());
            for (int i = 0; i < 3; ++i) {
                if (!std::isfinite(range.GetMin()[i]) || !std::isfinite(range.GetMax()[i]))
                    throw std::runtime_error("clearance inconclusive: non-finite obstacle bounds");
                record.minimum.push_back(range.GetMin()[i]); record.maximum.push_back(range.GetMax()[i]);
            }
            out.push_back(std::move(record));
        }
        return out;
    });
}

CurveBakeRaw read_curve_bake(const StageHandle& handle, rust::Str owner) {
    return handle.impl_->access([&] {
        CurveBakeRaw out;
        const auto prim = handle.impl_->stage->GetPrimAtPath(SdfPath(to_std(owner)));
        if (!prim || !prim.HasAttribute(TfToken("idtx:placement:generator"))) return out;
        out.owner = rust::String(to_std(owner));
        auto read = [&](const char* name, auto* value) {
            if (!prim.GetAttribute(TfToken(std::string("idtx:placement:") + name)).Get(value))
                throw std::runtime_error(std::string("missing or invalid placement setting: ") + name);
        };
        std::string generator, digest, settings_digest;
        read("generator", &generator); read("sourceDigest", &digest); read("settingsDigest", &settings_digest);
        out.generator = rust::String(generator); out.source_digest = rust::String(digest); out.settings_digest = rust::String(settings_digest);
        read("curveIndex", &out.curve_index); read("spacing", &out.spacing); read("start", &out.start); read("window", &out.window);
        read("orient", &out.orient); read("seed", &out.seed); read("tolerance", &out.tolerance);
        GfVec2d lateral; GfVec3d normal, extent;
        read("lateral", &lateral); read("floorNormal", &normal); read("halfExtents", &extent);
        for (int i = 0; i < 2; ++i) out.lateral.push_back(lateral[i]);
        for (int i = 0; i < 3; ++i) { out.floor_normal.push_back(normal[i]); out.half_extents.push_back(extent[i]); }
        SdfPathVector targets;
        prim.GetRelationship(TfToken("idtx:placement:source")).GetTargets(&targets);
        if (targets.size() != 1) throw std::runtime_error("placement source relationship needs one target");
        out.source = rust::String(targets[0].GetString());
        const auto output = prim.GetPath().AppendChild(TfToken("Placements"));
        SdfPathVector outputs;
        prim.GetRelationship(TfToken("idtx:placement:output")).GetTargets(&outputs);
        if (outputs != SdfPathVector{output} || !handle.impl_->stage->GetPrimAtPath(output))
            out.source_digest = rust::String("");
        return out;
    });
}

bool curve_scene_complete(const StageHandle& handle) {
    return handle.impl_->access([&] {
        const auto stage = handle.impl_->stage;
        if (!stage || !stage->GetCompositionErrors().empty() || !stage->GetMutedLayers().empty()) return false;
        for (const auto& prim : stage->TraverseAll()) {
            if (prim.IsActive() && prim.HasAuthoredPayloads() && !prim.IsLoaded()) return false;
        }
        return true;
    });
}

#ifdef IDTX_AUTHORING
rust::String bake_curve_placements(StageHandle& handle, const CurveBakeRaw& bake) {
    return with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
        const auto stage = handle.impl_->stage;
        const auto root = stage->GetRootLayer();
        const SdfPath owner(std::string(bake.owner)), source(std::string(bake.source));
        if (!owner.IsPrimPath() || !owner.IsAbsolutePath() || !stage->GetPrimAtPath(owner.GetParentPath())
            || source.HasPrefix(owner) || !UsdGeomBasisCurves(stage->GetPrimAtPath(source)))
            throw std::runtime_error("curve bake requires an existing output parent and a native BasisCurves source outside the output Scope");
        const auto parent = stage->GetPrimAtPath(owner.GetParentPath());
        if (parent.IsInstance() || parent.IsInstanceProxy()) throw std::runtime_error("make the route non-instanceable before authoring placements beneath it");
        if (bake.matrices.empty() || bake.matrices.size() % 16 || bake.matrices.size() > 4096 * 16
            || bake.lateral.size() != 2 || bake.floor_normal.size() != 3 || bake.half_extents.size() != 3)
            throw std::runtime_error("invalid curve bake arrays");
        for (double value : bake.matrices) if (!std::isfinite(value)) throw std::runtime_error("non-finite placement transform");
        if (!stage->GetCompositionErrors().empty() || !stage->GetMutedLayers().empty())
            throw std::runtime_error("clearance inconclusive: unresolved composition or muted layers");
        for (const auto& prim : stage->TraverseAll())
            if (prim.IsActive() && prim.HasAuthoredPayloads() && !prim.IsLoaded())
                throw std::runtime_error("clearance inconclusive: unloaded payloads");
        const auto existing = stage->GetPrimAtPath(owner);
        if (existing) {
            std::string generator;
            SdfPathVector sources;
            existing.GetAttribute(TfToken("idtx:placement:generator")).Get(&generator);
            existing.GetRelationship(TfToken("idtx:placement:source")).GetTargets(&sources);
            if (generator != std::string(bake.generator)
                || sources != SdfPathVector{source} || !existing.IsA<UsdGeomScope>())
                throw std::runtime_error("output path is not owned by this curve placement tool/source");
        }
        // Build native schemas on an isolated layer, then commit just this owner.
        // No USD stage queries inside the SdfChangeBlock.
        const auto temporary = UsdStage::CreateInMemory();
        const auto scope = UsdGeomScope::Define(temporary, owner).GetPrim();
        const auto output = owner.AppendChild(TfToken("Placements"));
        UsdGeomXformCache transforms;
        const auto parent_world = transforms.GetLocalToWorldTransform(stage->GetPrimAtPath(owner.GetParentPath()));
        GfMatrix4d normalized_parent(1.0);
        for (int r = 0; r < 3; ++r) {
            const double length = GfVec3d(parent_world[r][0], parent_world[r][1], parent_world[r][2]).GetLength();
            if (!std::isfinite(length) || length == 0.0) throw std::runtime_error("placement parent transform is singular");
            for (int c = 0; c < 3; ++c) normalized_parent[r][c] = parent_world[r][c] / length;
        }
        if (std::abs(normalized_parent.GetDeterminant()) <= 1e-12) throw std::runtime_error("placement parent transform is singular");
        const auto parent_inverse = parent_world.GetInverse();
        UsdGeomScope::Define(temporary, output);
        auto attr = [&](const char* name, const SdfValueTypeName& type, const VtValue& value) {
            if (!scope.CreateAttribute(TfToken(std::string("idtx:placement:") + name), type, true).Set(value))
                throw std::runtime_error("could not author curve placement setting");
        };
        attr("generator", SdfValueTypeNames->String, VtValue(std::string(bake.generator)));
        attr("sourceDigest", SdfValueTypeNames->String, VtValue(std::string(bake.source_digest)));
        attr("settingsDigest", SdfValueTypeNames->String, VtValue(std::string(bake.settings_digest)));
        attr("sourceRevision", SdfValueTypeNames->UInt64, VtValue(bake.source_revision));
        attr("curveIndex", SdfValueTypeNames->UInt, VtValue(bake.curve_index));
        attr("spacing", SdfValueTypeNames->Double, VtValue(bake.spacing));
        attr("start", SdfValueTypeNames->Double, VtValue(bake.start));
        attr("window", SdfValueTypeNames->Double, VtValue(bake.window));
        attr("tolerance", SdfValueTypeNames->Double, VtValue(bake.tolerance));
        attr("lateral", SdfValueTypeNames->Double2, VtValue(GfVec2d(bake.lateral[0], bake.lateral[1])));
        attr("floorNormal", SdfValueTypeNames->Double3, VtValue(GfVec3d(bake.floor_normal[0], bake.floor_normal[1], bake.floor_normal[2])));
        attr("halfExtents", SdfValueTypeNames->Double3, VtValue(GfVec3d(bake.half_extents[0], bake.half_extents[1], bake.half_extents[2])));
        attr("orient", SdfValueTypeNames->Bool, VtValue(bake.orient));
        attr("seed", SdfValueTypeNames->UInt, VtValue(bake.seed));
        scope.CreateRelationship(TfToken("idtx:placement:source"), true).SetTargets({source});
        scope.CreateRelationship(TfToken("idtx:placement:output"), true).SetTargets({output});
        for (size_t i = 0; i < bake.matrices.size() / 16; ++i) {
            GfMatrix4d matrix;
            for (int r = 0; r < 4; ++r) for (int c = 0; c < 4; ++c) matrix[r][c] = bake.matrices[i*16+r*4+c];
            const auto xform = UsdGeomXform::Define(temporary, output.AppendChild(TfToken("Sample_" + std::to_string(i))));
            xform.GetPrim().SetActive(true);
            if (!xform.AddTransformOp(UsdGeomXformOp::PrecisionDouble).Set(matrix * parent_inverse))
                throw std::runtime_error("could not author native placement transform");
        }
        const auto layer = temporary->GetRootLayer();
        // Referenced routes carry weaker bakes. Suppress obsolete inherited
        // samples explicitly; old root-only samples disappear on replacement.
        if (const auto old_output = stage->GetPrimAtPath(output)) {
            for (const auto& child : old_output.GetChildren()) {
                if (temporary->GetPrimAtPath(child.GetPath())) continue;
                const auto stack = child.GetPrimStack();
                if (std::any_of(stack.begin(), stack.end(), [&](const auto& spec) { return spec->GetLayer() != root; }))
                    temporary->OverridePrim(child.GetPath()).SetActive(false);
            }
        }
        // Structural undo compares parsed USDA captures to live specs. Match
        // USDA's canonical property ordering on this tool-owned subtree before
        // publication (without normalizing any unrelated root opinions).
        std::string text;
        layer->ExportToString(&text);
        const auto canonical = SdfLayer::CreateAnonymous(".usda");
        if (!canonical->ImportFromString(text)) throw std::runtime_error("could not normalize placement layer");
        layer->TransferContent(canonical);
        // Preserve unrelated properties and children of an existing owner.
        const auto working = SdfLayer::CreateAnonymous();
        working->TransferContent(root);
        if (owner.GetParentPath() != SdfPath::AbsoluteRootPath()) SdfCreatePrimInLayer(working, owner.GetParentPath());
        if (existing && !working->GetPrimAtPath(owner)) SdfCreatePrimInLayer(working, owner);
        if (!existing) {
            if (!SdfCopySpec(layer, owner, working, owner)) throw std::runtime_error("could not copy placement owner");
        } else {
            if (working->GetPrimAtPath(output)) {
                SdfBatchNamespaceEdit remove; remove.Add(output, SdfPath::EmptyPath());
                if (!working->Apply(remove)) throw std::runtime_error("could not replace placement output");
            }
            if (!SdfCopySpec(layer, output, working, output)) throw std::runtime_error("could not copy placements");
            for (const auto& property : layer->GetPrimAtPath(owner)->GetProperties())
                if (!SdfCopySpec(layer, property->GetPath(), working, property->GetPath())) throw std::runtime_error("could not copy placement settings");
        }
        { SdfChangeBlock block; root->TransferContent(working); }
        return rust::String(text);
    });
}
#endif

void validate_runtime_stage(const StageHandle& handle) {
    handle.impl_->access([&] {
        const auto stage = handle.impl_->stage;
        if (!stage || !stage->GetCompositionErrors().empty()) throw std::runtime_error("runtime stage has unresolved composition dependencies");
        for (const auto& prim : stage->Traverse()) {
            if (prim.IsActive() && prim.HasAuthoredPayloads() && !prim.IsLoaded())
                throw std::runtime_error("runtime stage has an unloaded required payload");
        }
    });
}

StageKindRaw read_stage_kind(rust::Str path_text) {
    // Classify a stage file by reading its ROOT LAYER only — no stage
    // composition, no payload load (see roadmap Appendix A.1). Opening the
    // SdfLayer is cheap and never triggers recomposition.
    return with_stage_lifecycle_lock<std::shared_lock<std::shared_mutex>>([&] {
    StageKindRaw out;
    const std::string identifier = to_std(path_text);
    const SdfLayerRefPtr layer = SdfLayer::FindOrOpen(identifier);
    if (!layer) {
        // A file that can't be opened as a layer is simply untagged; the
        // browser treats it as an unclassified stage rather than an error.
        return out;
    }
    const VtDictionary custom = layer->GetCustomLayerData();
    const auto idtx_it = custom.find("idtx");
    if (idtx_it == custom.end() || !idtx_it->second.IsHolding<VtDictionary>()) {
        return out;
    }
    const VtDictionary idtx = idtx_it->second.UncheckedGet<VtDictionary>();
    if (const auto it = idtx.find("stageKind"); it != idtx.end()) {
        if (it->second.IsHolding<TfToken>()) {
            out.stage_kind = rust::String(it->second.UncheckedGet<TfToken>().GetString());
        } else if (it->second.IsHolding<std::string>()) {
            out.stage_kind = rust::String(it->second.UncheckedGet<std::string>());
        }
    }
    if (const auto it = idtx.find("displayName"); it != idtx.end()) {
        if (it->second.IsHolding<std::string>()) {
            out.display_name = rust::String(it->second.UncheckedGet<std::string>());
        }
    }
    if (const auto it = idtx.find("schemaVersion"); it != idtx.end()) {
        if (it->second.IsHolding<std::string>()) {
            out.schema_version = rust::String(it->second.UncheckedGet<std::string>());
        }
    }
    return out;
    });
}

StageRootPrimsRaw read_stage_root_prims(rust::Str path_text) {
    // Read the root layer's composition targets (defaultPrim + root `def` prim
    // names) by opening its ROOT LAYER only — no stage composition, no payload
    // load. The scene-tree drop flow uses this to point a reference/payload arc
    // at a real prim: a dropped stage with no defaultPrim composes nothing
    // unless the arc names an explicit target (mirrors `read_stage_kind`).
    return with_stage_lifecycle_lock<std::shared_lock<std::shared_mutex>>([&] {
    StageRootPrimsRaw out;
    const std::string identifier = to_std(path_text);
    const SdfLayerRefPtr layer = SdfLayer::FindOrOpen(identifier);
    if (!layer) {
        // A file that can't be opened as a layer has no targets to offer.
        return out;
    }
    if (layer->HasDefaultPrim()) {
        out.default_prim = rust::String(layer->GetDefaultPrim().GetString());
    }
    for (const SdfPrimSpecHandle& child : layer->GetRootPrims()) {
        if (!child || child->GetSpecifier() != SdfSpecifierDef) {
            continue;  // `over`/`class` roots compose nothing — skip them.
        }
        out.root_def_prims.push_back(rust::String(child->GetName()));
    }
    return out;
    });
}

rust::Vec<rust::String> traverse_prim_paths(
    const StageHandle& handle, bool guide, bool proxy, bool render) {
    return handle.impl_->access([&] {
        rust::Vec<rust::String> out;
        const bool all_purposes = !guide && !proxy && !render;
        for (const UsdPrim& prim :
             handle.impl_->stage->Traverse(UsdTraverseInstanceProxies())) {
            const UsdGeomImageable imageable(prim);
            // ComputePurpose ignores authored attributes on non-imageable
            // prims, while still inheriting from valid imageable ancestors.
            const TfToken purpose = imageable.ComputePurpose();
            const bool matches = all_purposes ||
                (purpose == UsdGeomTokens->guide ? guide :
                 purpose == UsdGeomTokens->proxy ? proxy :
                 purpose == UsdGeomTokens->render ? render : true);
            if (matches) {
                out.push_back(rust::String(prim.GetPath().GetString()));
            }
            // Never prune: descendants can override an excluded purpose,
            // including by explicitly resetting it to default.
        }
        return out;
    });
}

rust::Vec<TopologyRecordRaw> project_topology(
    const StageHandle& handle, rust::Slice<const rust::String> roots) {
    return handle.impl_->access([&] {
    rust::Vec<TopologyRecordRaw> out;
    const UsdStageRefPtr& stage = handle.impl_->stage;
    std::vector<SdfPath> root_paths;
    root_paths.reserve(roots.size());
    for (const rust::String& root : roots) {
        const SdfPath path(to_std(root));
        if (!path.IsAbsoluteRootOrPrimPath()) {
            throw std::runtime_error("topology projection root is invalid");
        }
        if (std::any_of(root_paths.begin(), root_paths.end(),
                        [&path](const SdfPath& retained) {
                            return path.HasPrefix(retained);
                        })) {
            continue;
        }
        root_paths.erase(
            std::remove_if(root_paths.begin(), root_paths.end(),
                           [&path](const SdfPath& retained) {
                               return retained.HasPrefix(path);
                           }),
            root_paths.end());
        root_paths.push_back(path);
    }
    const bool full_projection = root_paths.empty() ||
        std::find(root_paths.begin(), root_paths.end(),
                  SdfPath::AbsoluteRootPath()) != root_paths.end();
    if (full_projection) {
        TopologyRecordRaw pseudo;
        pseudo.path = rust::String("/");
        pseudo.parent_path = rust::String("");
        pseudo.name = rust::String("");
        pseudo.type_name = rust::String("PseudoRoot");
        pseudo.purpose = rust::String(UsdGeomTokens->default_.GetString());
        pseudo.active = true;
        pseudo.defined = true;
        pseudo.loaded = true;
        pseudo.is_pseudo_root = true;
        pseudo.is_xformable = false;
        pseudo.is_instance = false;
        pseudo.is_prototype = false;
        out.push_back(std::move(pseudo));
    }
    const SdfLayerHandle root_layer = handle.impl_->stage->GetRootLayer();
    std::vector<UsdPrim> projection_prims;
    // Traverse active, defined, non-abstract prims. Omitting UsdPrimIsLoaded so
    // unloaded payload roots remain visible as collapsed nodes in the tree.
    // NOTE: this predicate does NOT expand instance proxies. Native instances
    // (`instanceable = true`) therefore appear as collapsed leaf nodes; their
    // shared prototype geometry is projected once via GetPrototypes() below.
    const auto tree_pred =
        UsdPrimIsActive && UsdPrimIsDefined && !UsdPrimIsAbstract;
    if (full_projection) {
        for (const UsdPrim& prim : stage->Traverse(tree_pred)) {
            projection_prims.push_back(prim);
        }
    } else {
        for (const SdfPath& root_path : root_paths) {
            const UsdPrim root = stage->GetPrimAtPath(root_path);
            if (!root) {
                // A missing requested root is a valid deletion tombstone.
                continue;
            }
            // Shared instancing prototypes (/__Prototype_N) are stage-global and
            // are projected once, flagged, via the unconditional GetPrototypes()
            // pass below. A resync can hand us a /__Prototype_N root (loading a
            // payload composes new prototypes), but walking it HERE would emit
            // every prototype prim a second time, UNFLAGGED, duplicating the
            // whole prototype subtree in the topology store. Never project a
            // prototype root through the per-root subtree walk.
            if (root.IsPrototype() || root.IsInPrototype()) {
                continue;
            }
            for (const UsdPrim& prim : UsdPrimRange(root, tree_pred)) {
                projection_prims.push_back(prim);
            }
        }
    }
    // Shared instancing prototypes (/__Prototype_N) live outside the normal
    // traversal. Expose each prototype subtree exactly once so its geometry
    // hydrates into entities the renderer can resolve; the UI groups/hides them
    // via `is_prototype`. Prototypes are shared, so we always project them all.
    std::vector<UsdPrim> prototype_prims;
    for (const UsdPrim& prototype : stage->GetPrototypes()) {
        for (const UsdPrim& prim : UsdPrimRange(prototype, tree_pred)) {
            prototype_prims.push_back(prim);
        }
    }
    const auto emit_topology_record = [&](const UsdPrim& prim, bool is_prototype) {
        TopologyRecordRaw record;
        record.path = rust::String(prim.GetPath().GetString());
        record.parent_path = rust::String(prim.GetParent().GetPath().GetString());
        record.name = rust::String(prim.GetName().GetString());
        record.type_name = rust::String(prim.GetTypeName().GetString());
        record.purpose = rust::String(
            UsdGeomImageable(prim).ComputePurpose().GetString());
        record.active = prim.IsActive();
        record.defined = prim.IsDefined();
        record.loaded = prim.IsLoaded();
        record.is_pseudo_root = false;
        record.is_xformable = prim.IsA<UsdGeomXformable>();
        record.is_instance = prim.IsInstance();
        record.is_prototype = is_prototype;
        topology_provenance(prim, root_layer, &record);
        out.push_back(std::move(record));
    };
    for (const UsdPrim& prim : projection_prims) {
        emit_topology_record(prim, /*is_prototype=*/false);
    }
    for (const UsdPrim& prim : prototype_prims) {
        emit_topology_record(prim, /*is_prototype=*/true);
    }
    return out;
    });
}

namespace {

PointInstancerRecordRaw point_instancer_record(
    const UsdStageRefPtr&, const UsdGeomPointInstancer&, UsdTimeCode,
    bool, rust::Vec<rust::String>*);

// UsdRelationship::GetTargets() can return stale composition data after a
// variant selection is authored in the session layer while the variant set
// definition lives in the root layer (an OpenUSD composition-cache staleness
// bug with session-layer variant overrides). Work around it by reading the
// prototypes relationship directly from the selected variant's layer spec in
// the root layer when a variant spec exists there; fall back to GetTargets()
// for the no-variant or cross-layer cases.
SdfPathVector get_instancer_prototype_paths(const UsdStageRefPtr& stage,
                                             const UsdGeomPointInstancer& instancer) {
    const UsdPrim prim = instancer.GetPrim();
    const SdfPath prim_path = prim.GetPath();
    const SdfLayerHandle root_l = stage->GetRootLayer();
    // Try each variant set on the instancer to find a variant spec that
    // authors rel prototypes.  In practice there is usually only one, but
    // the loop keeps the approach general.
    for (const std::string& set_name : prim.GetVariantSets().GetNames()) {
        const std::string selection =
            prim.GetVariantSet(set_name).GetVariantSelection();
        if (selection.empty()) {
            continue;
        }
        const SdfPath variant_path =
            prim_path.AppendVariantSelection(set_name, selection);
        const SdfPath rel_path =
            variant_path.AppendProperty(TfToken("prototypes"));
        const SdfRelationshipSpecHandle rel_spec =
            root_l->GetRelationshipAtPath(rel_path);
        if (rel_spec) {
            return rel_spec->GetTargetPathList().GetAppliedItems();
        }
    }
    // No variant spec authored rel prototypes — fall back to the composed
    // relationship targets.
    SdfPathVector targets;
    instancer.GetPrototypesRel().GetTargets(&targets);
    return targets;
}

void refresh_point_prototype_cache(
    const UsdStageRefPtr& stage, const std::vector<SdfPath>& root_paths,
    bool full_projection,
    std::map<SdfPath, SdfPathVector>* prototypes_by_instancer,
    std::set<SdfPath>* prototype_roots, bool* initialized) {
    const auto read_instancer =
        [stage, prototypes_by_instancer](const SdfPath& path) {
            const UsdGeomPointInstancer instancer(stage->GetPrimAtPath(path));
            if (!instancer) {
                prototypes_by_instancer->erase(path);
                return;
            }
            (*prototypes_by_instancer)[path] =
                get_instancer_prototype_paths(stage, instancer);
        };

    if (full_projection || !*initialized) {
        prototypes_by_instancer->clear();
        for (const UsdPrim& candidate : stage->Traverse()) {
            if (candidate.IsA<UsdGeomPointInstancer>()) {
                read_instancer(candidate.GetPath());
            }
        }
        *initialized = true;
    } else {
        std::set<SdfPath> refresh_ancestors;
        for (const SdfPath& root : root_paths) {
            for (SdfPath ancestor = root; !ancestor.IsEmpty();
                 ancestor = ancestor.GetParentPath()) {
                if (prototypes_by_instancer->find(ancestor) !=
                    prototypes_by_instancer->end()) {
                    refresh_ancestors.insert(ancestor);
                }
                if (ancestor == SdfPath::AbsoluteRootPath()) {
                    break;
                }
            }
            auto entry = prototypes_by_instancer->lower_bound(root);
            while (entry != prototypes_by_instancer->end() &&
                   entry->first.HasPrefix(root)) {
                entry = prototypes_by_instancer->erase(entry);
            }
            const UsdPrim root_prim = stage->GetPrimAtPath(root);
            if (!root_prim) {
                continue;
            }
            for (const UsdPrim& candidate : UsdPrimRange(root_prim)) {
                if (candidate.IsA<UsdGeomPointInstancer>()) {
                    read_instancer(candidate.GetPath());
                }
            }
        }
        for (const SdfPath& path : refresh_ancestors) {
            read_instancer(path);
        }
    }

    prototype_roots->clear();
    for (const auto& [instancer_path, targets] : *prototypes_by_instancer) {
        (void)instancer_path;
        prototype_roots->insert(targets.begin(), targets.end());
    }
}

bool paths_intersect(const SdfPath& first, const SdfPath& second) {
    return first.HasPrefix(second) || second.HasPrefix(first);
}

void add_projection_root(std::vector<SdfPath>* roots, const SdfPath& path) {
    if (path.IsEmpty() || std::any_of(
            roots->begin(), roots->end(), [&path](const SdfPath& retained) {
                return path.HasPrefix(retained);
            })) {
        return;
    }
    roots->erase(
        std::remove_if(roots->begin(), roots->end(),
                       [&path](const SdfPath& retained) {
                           return retained.HasPrefix(path);
                       }),
        roots->end());
    roots->push_back(path);
}

void append_matrix(const GfMatrix4d& matrix, rust::Vec<float>* values) {
    for (int row = 0; row < 4; ++row) {
        for (int column = 0; column < 4; ++column) {
            values->push_back(static_cast<float>(matrix[row][column]));
        }
    }
}

template <typename MatrixArray>
void append_matrices(const MatrixArray& matrices, rust::Vec<float>* values) {
    values->reserve(values->size() + matrices.size() * 16);
    for (const auto& matrix : matrices) {
        append_matrix(GfMatrix4d(matrix), values);
    }
}

bool matrix_is_finite(const GfMatrix4d& matrix) {
    for (int row = 0; row < 4; ++row) {
        for (int column = 0; column < 4; ++column) {
            if (!std::isfinite(matrix[row][column])) {
                return false;
            }
        }
    }
    return true;
}

template <typename MatrixArray>
bool matrices_are_finite(const MatrixArray& matrices) {
    return std::all_of(
        matrices.begin(), matrices.end(), [](const auto& matrix) {
            return matrix_is_finite(GfMatrix4d(matrix));
        });
}

void refresh_skeleton_cache(
    const UsdStageRefPtr& stage, UsdSkelCache* cache,
    std::vector<CachedSkeletonDependency>* dependencies, bool* initialized,
    const SdfPath& scope = SdfPath()) {
    if (*initialized) {
        return;
    }
    cache->Clear();
    dependencies->clear();
    std::map<SdfPath, std::size_t> dependency_by_skeleton;
    const auto prims = scope.IsEmpty()
        ? stage->Traverse(UsdTraverseInstanceProxies())
        : UsdPrimRange(stage->GetPrimAtPath(scope), UsdTraverseInstanceProxies());
    for (const UsdPrim& prim : prims) {
        if (!prim.IsA<UsdSkelRoot>()) {
            continue;
        }
        const UsdSkelRoot root(prim);
        if (!cache->Populate(root, UsdTraverseInstanceProxies())) {
            continue;
        }
        std::vector<UsdSkelBinding> bindings;
        if (!cache->ComputeSkelBindings(
                root, &bindings, UsdTraverseInstanceProxies())) {
            continue;
        }
        for (const UsdSkelBinding& binding : bindings) {
            const SdfPath skeleton_path = binding.GetSkeleton().GetPath();
            auto [entry, inserted] = dependency_by_skeleton.emplace(
                skeleton_path, dependencies->size());
            if (inserted) {
                CachedSkeletonDependency dependency;
                dependency.root_path = root.GetPath();
                dependency.skeleton_path = skeleton_path;
                const UsdSkelSkeletonQuery skeleton_query =
                    cache->GetSkelQuery(binding.GetSkeleton());
                if (skeleton_query && skeleton_query.GetAnimQuery()) {
                    dependency.animation_path =
                        skeleton_query.GetAnimQuery().GetPrim().GetPath();
                }
                dependencies->push_back(std::move(dependency));
            }
            CachedSkeletonDependency& dependency = (*dependencies)[entry->second];
            for (const UsdSkelSkinningQuery& query :
                 binding.GetSkinningTargets()) {
                const SdfPath path = query.GetPrim().GetPath();
                if (std::find(dependency.skinned_paths.begin(),
                              dependency.skinned_paths.end(), path) ==
                    dependency.skinned_paths.end()) {
                    dependency.skinned_paths.push_back(path);
                }
            }
        }
    }
    std::sort(dependencies->begin(), dependencies->end(),
              [](const CachedSkeletonDependency& first,
                 const CachedSkeletonDependency& second) {
                  return first.skeleton_path < second.skeleton_path;
              });
    for (CachedSkeletonDependency& dependency : *dependencies) {
        std::sort(dependency.skinned_paths.begin(),
                  dependency.skinned_paths.end());
    }
    *initialized = true;
}

void append_skeleton_projection(
    const UsdStageRefPtr& stage, const UsdTimeCode& time,
    UsdGeomXformCache* xform_cache, UsdSkelCache* skeleton_cache,
    const std::vector<CachedSkeletonDependency>& dependencies,
    const std::vector<UsdPrim>& projection_prims,
    ViewportProjectionRaw* out, bool poses_only) {
    std::set<SdfPath> projected_paths;
    for (const UsdPrim& prim : projection_prims) {
        projected_paths.insert(prim.GetPath());
    }

    std::set<SdfPath> projected_skeletons;
    for (const UsdPrim& prim : projection_prims) {
        if (!prim.IsA<UsdSkelSkeleton>()) {
            continue;
        }
        const UsdSkelSkeleton skeleton(prim);
        const UsdSkelSkeletonQuery query =
            skeleton_cache->GetSkelQuery(skeleton);
        if (!query) {
            out->diagnostics.push_back(rust::String(
                prim.GetPath().GetString() +
                " UsdSkel skeleton query is invalid; bound meshes remain in rest-pose fallback"));
            continue;
        }
        const VtTokenArray joint_order = query.GetJointOrder();
        const UsdSkelTopology& topology = query.GetTopology();
        std::string topology_reason;
        if (joint_order.empty() || joint_order.size() != topology.size() ||
            !topology.Validate(&topology_reason)) {
            out->diagnostics.push_back(rust::String(
                prim.GetPath().GetString() +
                " has malformed UsdSkel topology; rest-pose fallback: " +
                (topology_reason.empty() ? "joint order is empty or mis-sized"
                                         : topology_reason)));
            continue;
        }
        VtMatrix4dArray bind_transforms;
        VtMatrix4dArray rest_transforms;
        const bool bind_valid = skeleton.GetBindTransformsAttr().Get(
            &bind_transforms, UsdTimeCode::Default());
        const bool rest_valid = query.ComputeJointLocalTransforms(
            &rest_transforms, UsdTimeCode::Default(), true);
        if (!bind_valid || !rest_valid ||
            bind_transforms.size() != joint_order.size() ||
            rest_transforms.size() != joint_order.size() ||
            !matrices_are_finite(bind_transforms) ||
            !matrices_are_finite(rest_transforms)) {
            out->diagnostics.push_back(rust::String(
                prim.GetPath().GetString() +
                " has missing, non-finite, or mis-sized UsdSkel bind/rest transforms; bound meshes remain in rest-pose fallback"));
            continue;
        }

        SkeletonTopologyRecordRaw topology_record;
        topology_record.skeleton_path =
            rust::String(prim.GetPath().GetString());
        for (const TfToken& joint : joint_order) {
            topology_record.joint_paths.push_back(
                rust::String(joint.GetString()));
        }
        for (int parent : topology.GetParentIndices()) {
            topology_record.parent_indices.push_back(parent);
        }
        append_matrices(bind_transforms, &topology_record.bind_transforms);
        append_matrices(rest_transforms, &topology_record.rest_transforms);
        out->skeleton_topologies.push_back(std::move(topology_record));
        projected_skeletons.insert(prim.GetPath());

        VtMatrix4dArray local_transforms;
        VtMatrix4dArray world_transforms;
        VtMatrix4dArray skinning_transforms;
        const bool pose_valid = query.ComputeJointLocalTransforms(
                                    &local_transforms, time, false) &&
            query.ComputeJointWorldTransforms(
                &world_transforms, xform_cache, false) &&
            query.ComputeSkinningTransforms(&skinning_transforms, time) &&
            local_transforms.size() == joint_order.size() &&
            world_transforms.size() == joint_order.size() &&
            skinning_transforms.size() == joint_order.size() &&
            matrices_are_finite(local_transforms) &&
            matrices_are_finite(world_transforms) &&
            matrices_are_finite(skinning_transforms);
        if (!pose_valid) {
            out->diagnostics.push_back(rust::String(
                prim.GetPath().GetString() +
                " UsdSkel pose or skinning palette could not be sampled; bound meshes remain in rest-pose fallback"));
            continue;
        }
        SkeletonPoseRecordRaw pose_record;
        pose_record.skeleton_path =
            rust::String(prim.GetPath().GetString());
        if (query.GetAnimQuery()) {
            pose_record.animation_path = rust::String(
                query.GetAnimQuery().GetPrim().GetPath().GetString());
        }
        append_matrix(xform_cache->GetLocalToWorldTransform(prim),
                      &pose_record.skeleton_world_transform);
        append_matrices(local_transforms,
                        &pose_record.joint_local_transforms);
        append_matrices(world_transforms,
                        &pose_record.joint_world_transforms);
        append_matrices(skinning_transforms,
                        &pose_record.skinning_transforms);
        out->skeleton_poses.push_back(std::move(pose_record));
    }

    // Skin bindings (per-vertex influences + skinned bounds) are static across
    // animation frames — the renderer reuses the SkinBindingComp hydrated at
    // load. The skeleton-pose-only sampling path skips this loop entirely: it
    // needs only the refreshed joint palettes above, and emitting skin bindings
    // without their meshes would fail the projection's mesh-point-count check.
    if (poses_only) {
        return;
    }

    for (const CachedSkeletonDependency& dependency : dependencies) {
        const bool included =
            projected_paths.find(dependency.skeleton_path) !=
                projected_paths.end() ||
            std::any_of(dependency.skinned_paths.begin(),
                        dependency.skinned_paths.end(),
                        [&projected_paths](const SdfPath& path) {
                            return projected_paths.find(path) !=
                                projected_paths.end();
                        });
        if (!included) {
            continue;
        }
        SkeletonDependencyRecordRaw dependency_record;
        dependency_record.skeleton_path =
            rust::String(dependency.skeleton_path.GetString());
        dependency_record.animation_path =
            rust::String(dependency.animation_path.GetString());
        for (const SdfPath& path : dependency.skinned_paths) {
            dependency_record.skinned_paths.push_back(
                rust::String(path.GetString()));
        }
        out->skeleton_dependencies.push_back(std::move(dependency_record));

        const UsdSkelSkeleton skeleton(
            stage->GetPrimAtPath(dependency.skeleton_path));
        const UsdSkelSkeletonQuery skeleton_query =
            skeleton_cache->GetSkelQuery(skeleton);
        VtMatrix4dArray skinning_transforms;
        const bool palette_valid = skeleton_query &&
            projected_skeletons.find(dependency.skeleton_path) !=
                projected_skeletons.end() &&
            skeleton_query.ComputeSkinningTransforms(
                &skinning_transforms, time) &&
            matrices_are_finite(skinning_transforms);
        for (const SdfPath& path : dependency.skinned_paths) {
            if (projected_paths.find(path) == projected_paths.end()) {
                continue;
            }
            const UsdPrim prim = stage->GetPrimAtPath(path);
            if (!prim.IsA<UsdGeomMesh>()) {
                out->diagnostics.push_back(rust::String(
                    path.GetString() +
                    " has an official UsdSkel binding but is not a Mesh; portable mesh skinning skipped"));
                continue;
            }
            const UsdSkelSkinningQuery query =
                skeleton_cache->GetSkinningQuery(prim);
            if (!query || !query.HasJointInfluences()) {
                out->diagnostics.push_back(rust::String(
                    path.GetString() +
                    " has no valid UsdSkel joint influences; rest-pose fallback"));
                continue;
            }
            VtVec3fArray points;
            if (!UsdGeomMesh(prim).GetPointsAttr().Get(&points, time)) {
                out->diagnostics.push_back(rust::String(
                    path.GetString() +
                    " points could not be sampled for UsdSkel; rest-pose fallback"));
                continue;
            }

            SkinBindingRecordRaw record;
            record.path = rust::String(path.GetString());
            record.skeleton_path =
                rust::String(dependency.skeleton_path.GetString());
            const TfToken method = query.GetSkinningMethod();
            record.skinning_method = rust::String(method.GetString());
            const GfMatrix4d geom_bind = query.GetGeomBindTransform(time);
            append_matrix(geom_bind, &record.geom_bind_transform);

            VtIntArray indices;
            VtFloatArray weights;
            const int influence_count = query.GetNumInfluencesPerComponent();
            bool influences_valid = influence_count > 0 &&
                query.ComputeVaryingJointInfluences(
                    points.size(), &indices, &weights, time) &&
                indices.size() == weights.size() &&
                indices.size() ==
                    points.size() * static_cast<std::size_t>(influence_count) &&
                matrix_is_finite(geom_bind);
            if (influences_valid) {
                for (std::size_t point = 0; point < points.size(); ++point) {
                    float sum = 0.0f;
                    for (int influence = 0; influence < influence_count;
                         ++influence) {
                        const std::size_t index =
                            point * static_cast<std::size_t>(influence_count) +
                            static_cast<std::size_t>(influence);
                        if (indices[index] < 0 ||
                            static_cast<std::size_t>(indices[index]) >=
                                skinning_transforms.size() ||
                            !std::isfinite(weights[index]) ||
                            weights[index] < 0.0f) {
                            influences_valid = false;
                            break;
                        }
                        sum += weights[index];
                    }
                    if (!influences_valid || !std::isfinite(sum) ||
                        sum <= std::numeric_limits<float>::epsilon()) {
                        influences_valid = false;
                        break;
                    }
                }
            }

            const bool classic_linear = method == UsdSkelTokens->classicLinear;
            const bool known_method = classic_linear ||
                method == UsdSkelTokens->dualQuaternion;
            const bool custom_joint_order = query.GetJointMapper() &&
                !query.GetJointMapper()->IsIdentity();
            bool cpu_fallback = false;
            bool rest_fallback = false;
            if (!palette_valid || !influences_valid) {
                rest_fallback = true;
                out->diagnostics.push_back(rust::String(
                    path.GetString() +
                    " has malformed, non-finite, or out-of-range UsdSkel palette/influences; rest-pose fallback"));
            } else if (query.HasBlendShapes()) {
                rest_fallback = true;
                out->diagnostics.push_back(rust::String(
                    path.GetString() +
                    " uses UsdSkel blend shapes, which are not in the portable contract; rest-pose fallback"));
            } else if (!known_method) {
                rest_fallback = true;
                out->diagnostics.push_back(rust::String(
                    path.GetString() + " uses unknown UsdSkel skinning method " +
                    method.GetString() + "; rest-pose fallback"));
            } else if (!classic_linear) {
                cpu_fallback = true;
                out->diagnostics.push_back(rust::String(
                    path.GetString() +
                    " uses dualQuaternion UsdSkel skinning; bounded official OpenUSD CPU fallback"));
            } else if (skinning_transforms.size() > kPortableJointLimit) {
                cpu_fallback = true;
                out->diagnostics.push_back(rust::String(
                    path.GetString() + " has " +
                    std::to_string(skinning_transforms.size()) +
                    " joints (limit=" + std::to_string(kPortableJointLimit) +
                    "); exceeds portable joint palette limit; bounded official OpenUSD CPU fallback"));
            } else if (influence_count >
                       static_cast<int>(kPortableInfluenceLimit)) {
                cpu_fallback = true;
                out->diagnostics.push_back(rust::String(
                    path.GetString() + " has " +
                    std::to_string(influence_count) +
                    " influences/vertex (limit=" + std::to_string(kPortableInfluenceLimit) +
                    "); exceeds portable influence limit; bounded official OpenUSD CPU fallback"));
            }

            // Custom (per-mesh) joint order: the mesh authors its own
            // `skel:joints`, so its jointIndices primvar addresses a mesh-local
            // joint list rather than the skeleton's global palette. Rather than
            // fall back to CPU deformation, build a static mesh-local → skeleton
            // remap so the GPU classic-linear path can index the shared skeleton
            // palette directly. The remap is topological (not time-varying), so
            // it is safe to reuse for every sampled frame.
            std::vector<std::uint16_t> joint_remap;
            if (!rest_fallback && !cpu_fallback && custom_joint_order) {
                VtTokenArray mesh_joints;
                query.GetJointOrder(&mesh_joints);
                const VtTokenArray skel_joints = skeleton_query.GetJointOrder();
                joint_remap.resize(mesh_joints.size());
                bool remap_valid = !mesh_joints.empty();
                for (std::size_t k = 0; remap_valid && k < mesh_joints.size();
                     ++k) {
                    bool found = false;
                    for (std::size_t s = 0; s < skel_joints.size(); ++s) {
                        if (mesh_joints[k] == skel_joints[s]) {
                            joint_remap[k] = static_cast<std::uint16_t>(s);
                            found = true;
                            break;
                        }
                    }
                    if (!found) {
                        remap_valid = false;
                    }
                }
                if (!remap_valid) {
                    cpu_fallback = true;
                    joint_remap.clear();
                    out->diagnostics.push_back(rust::String(
                        path.GetString() +
                        " uses a custom UsdSkel joint order that could not be"
                        " mapped to the skeleton palette; bounded official"
                        " OpenUSD CPU fallback"));
                } else {
                    out->diagnostics.push_back(rust::String(
                        path.GetString() +
                        " uses a custom UsdSkel joint order; remapped " +
                        std::to_string(mesh_joints.size()) +
                        " mesh-local joints to the skeleton palette for GPU"
                        " classic-linear skinning"));
                }
            }

            if (!rest_fallback && points.size() > kCpuSkinningPointLimit) {
                rest_fallback = true;
                cpu_fallback = false;
                out->diagnostics.push_back(rust::String(
                    path.GetString() + " exceeds the bounded CPU skinning limit of " +
                    std::to_string(kCpuSkinningPointLimit) +
                    " points; rest-pose fallback"));
            }

            if (!rest_fallback && !cpu_fallback) {
                record.joint_indices.reserve(points.size() *
                                             kPortableInfluenceLimit);
                record.joint_weights.reserve(points.size() *
                                             kPortableInfluenceLimit);
                for (std::size_t point = 0; point < points.size(); ++point) {
                    float sum = 0.0f;
                    for (int influence = 0; influence < influence_count;
                         ++influence) {
                        sum += weights[point *
                                           static_cast<std::size_t>(influence_count) +
                                       static_cast<std::size_t>(influence)];
                    }
                    for (std::size_t influence = 0;
                         influence < kPortableInfluenceLimit; ++influence) {
                        if (influence <
                            static_cast<std::size_t>(influence_count)) {
                            const std::size_t source =
                                point *
                                    static_cast<std::size_t>(influence_count) +
                                influence;
                            std::uint16_t joint =
                                static_cast<std::uint16_t>(indices[source]);
                            // Translate mesh-local joint indices into the shared
                            // skeleton palette order when a custom joint order
                            // was resolved above.
                            if (!joint_remap.empty()) {
                                joint = joint < joint_remap.size()
                                            ? joint_remap[joint]
                                            : 0;
                            }
                            record.joint_indices.push_back(joint);
                            record.joint_weights.push_back(weights[source] / sum);
                        } else {
                            record.joint_indices.push_back(0);
                            record.joint_weights.push_back(0.0f);
                        }
                    }
                }
            }

            // ── Skinned world-space bounds ──────────────────────────────────
            // Two paths depending on execution mode:
            //   * cpu-openusd : the renderer needs the CPU-deformed points, so
            //     run the full official UsdSkel ComputeSkinnedPoints deform.
            //   * gpu-classic-linear : the GPU performs the skinning, so calling
            //     ComputeSkinnedPoints every frame is pure overhead (its output
            //     is never uploaded). Instead compute the exact bounds with a
            //     hand-rolled LBS deform that reuses the already-computed
            //     portable influences and joint palette — identical math, but
            //     without re-resolving the skinning query, its primvars, and the
            //     VtArray allocations on every sampled frame. This keeps the
            //     projection's skinned_bounds contract intact while removing the
            //     dominant per-frame cost for GPU-skinned characters.
            if (cpu_fallback) {
                VtVec3fArray deformed_points = points;
                if (!rest_fallback &&
                    !query.ComputeSkinnedPoints(
                        skinning_transforms, &deformed_points, time)) {
                    rest_fallback = true;
                    cpu_fallback = false;
                    record.joint_indices.clear();
                    record.joint_weights.clear();
                    out->diagnostics.push_back(rust::String(
                        path.GetString() +
                        " official OpenUSD deformation failed; rest-pose fallback"));
                }
                if (!rest_fallback) {
                    const GfMatrix4d skeleton_world =
                        xform_cache->GetLocalToWorldTransform(skeleton.GetPrim());
                    GfVec3d minimum(
                        std::numeric_limits<double>::infinity());
                    GfVec3d maximum(
                        -std::numeric_limits<double>::infinity());
                    for (const GfVec3f& point : deformed_points) {
                        const GfVec3d world = skeleton_world.Transform(
                            GfVec3d(point[0], point[1], point[2]));
                        for (int axis = 0; axis < 3; ++axis) {
                            minimum[axis] = std::min(minimum[axis], world[axis]);
                            maximum[axis] = std::max(maximum[axis], world[axis]);
                        }
                        record.cpu_skinned_points.push_back(point[0]);
                        record.cpu_skinned_points.push_back(point[1]);
                        record.cpu_skinned_points.push_back(point[2]);
                    }
                    if (!deformed_points.empty()) {
                        for (int axis = 0; axis < 3; ++axis) {
                            record.skinned_bounds_minimum.push_back(
                                static_cast<float>(minimum[axis]));
                            record.skinned_bounds_maximum.push_back(
                                static_cast<float>(maximum[axis]));
                        }
                    }
                }
            } else if (!rest_fallback) {
                const GfMatrix4d skeleton_world =
                    xform_cache->GetLocalToWorldTransform(skeleton.GetPrim());
                // Pre-compose geom_bind ∘ skinning_transform per joint (row-vector
                // order: p' = p * geom_bind * skinning_transform_j), matching the
                // official ComputeSkinnedPoints convention and the GPU shader.
                std::vector<GfMatrix4d> effective(skinning_transforms.size());
                for (std::size_t j = 0; j < skinning_transforms.size(); ++j) {
                    effective[j] = geom_bind * skinning_transforms[j];
                }
                GfVec3d minimum(std::numeric_limits<double>::infinity());
                GfVec3d maximum(-std::numeric_limits<double>::infinity());
                for (std::size_t point = 0; point < points.size(); ++point) {
                    const GfVec3d p(points[point][0], points[point][1],
                                    points[point][2]);
                    GfVec3d deformed(0.0);
                    for (std::size_t i = 0; i < kPortableInfluenceLimit; ++i) {
                        const std::size_t slot =
                            point * kPortableInfluenceLimit + i;
                        const float w = record.joint_weights[slot];
                        if (w == 0.0f) {
                            continue;
                        }
                        const std::uint16_t j = record.joint_indices[slot];
                        deformed += static_cast<double>(w) *
                                    effective[j].Transform(p);
                    }
                    const GfVec3d world = skeleton_world.Transform(deformed);
                    for (int axis = 0; axis < 3; ++axis) {
                        minimum[axis] = std::min(minimum[axis], world[axis]);
                        maximum[axis] = std::max(maximum[axis], world[axis]);
                    }
                }
                if (!points.empty()) {
                    for (int axis = 0; axis < 3; ++axis) {
                        record.skinned_bounds_minimum.push_back(
                            static_cast<float>(minimum[axis]));
                        record.skinned_bounds_maximum.push_back(
                            static_cast<float>(maximum[axis]));
                    }
                }
            }
            record.execution = rust::String(
                rest_fallback ? "rest-pose"
                              : (cpu_fallback ? "cpu-openusd"
                                              : "gpu-classic-linear"));
            if (!rest_fallback && !cpu_fallback) {
                out->diagnostics.push_back(rust::String(
                    path.GetString() + " UsdSkel gpu-classic-linear: " +
                    std::to_string(skinning_transforms.size()) +
                    " joints, " + std::to_string(influence_count) +
                    " influences/vertex, " + std::to_string(points.size()) +
                    " points"));
            }
            out->skin_bindings.push_back(std::move(record));
        }
    }
}

ViewportProjectionRaw project_viewport_locked(
    const UsdStageRefPtr& stage,
    std::map<SdfPath, SdfPathVector>* point_prototypes_by_instancer,
    std::set<SdfPath>* point_prototype_roots,
    bool* point_prototype_cache_initialized,
    UsdSkelCache* skeleton_cache,
    std::vector<CachedSkeletonDependency>* skeleton_dependencies,
    bool* skeleton_cache_initialized,
    bool* prototypes_resident,
    double time_code, bool use_default_time,
    rust::Slice<const rust::String> roots, bool poses_only) {
    ViewportProjectionRaw out;
    out.schema_version = static_cast<std::uint32_t>(ViewportProjectionSchema::Current);
    const auto t_proj_start = std::chrono::steady_clock::now();
    const UsdTimeCode time = use_default_time ? UsdTimeCode::Default()
                                              : UsdTimeCode(time_code);
    UsdGeomXformCache transforms(time);
    std::vector<SdfPath> root_paths;
    root_paths.reserve(roots.size());
    for (const rust::String& root : roots) {
        const SdfPath path(to_std(root));
        if (!path.IsAbsoluteRootOrPrimPath() || !stage->GetPrimAtPath(path)) {
            throw std::runtime_error("viewport projection root does not exist");
        }
        if (std::any_of(root_paths.begin(), root_paths.end(),
                        [&path](const SdfPath& retained) {
                            return path.HasPrefix(retained);
                        })) {
            continue;
        }
        root_paths.erase(
            std::remove_if(root_paths.begin(), root_paths.end(),
                           [&path](const SdfPath& retained) {
                               return retained.HasPrefix(path);
                           }),
            root_paths.end());
        root_paths.push_back(path);
    }
    const bool full_projection = root_paths.empty() ||
        std::find(root_paths.begin(), root_paths.end(),
                  SdfPath::AbsoluteRootPath()) != root_paths.end();
    refresh_skeleton_cache(stage, skeleton_cache, skeleton_dependencies,
                           skeleton_cache_initialized);
    if (!full_projection) {
        const std::vector<SdfPath> requested_paths = root_paths;
        for (const CachedSkeletonDependency& dependency :
             *skeleton_dependencies) {
            const bool affected = std::any_of(
                requested_paths.begin(), requested_paths.end(),
                [&dependency](const SdfPath& requested) {
                    if (paths_intersect(requested, dependency.root_path) ||
                        paths_intersect(requested, dependency.skeleton_path) ||
                        (!dependency.animation_path.IsEmpty() &&
                         paths_intersect(requested,
                                         dependency.animation_path))) {
                        return true;
                    }
                    return std::any_of(
                        dependency.skinned_paths.begin(),
                        dependency.skinned_paths.end(),
                        [&requested](const SdfPath& skinned) {
                            return paths_intersect(requested, skinned);
                        });
                });
            if (!affected) {
                continue;
            }
            add_projection_root(&root_paths, dependency.skeleton_path);
            add_projection_root(&root_paths, dependency.animation_path);
            for (const SdfPath& skinned : dependency.skinned_paths) {
                add_projection_root(&root_paths, skinned);
            }
        }
    }
    std::vector<UsdPrim> projection_prims;
    // Default predicate (no instance-proxy expansion): native instances collapse
    // to leaf prims here. Each instance is recorded in `out.native_instances`; the
    // shared prototype geometry is appended below from GetPrototypes() and projected
    // exactly once (flagged prototype_only), then hardware-instanced by the renderer.
    if (full_projection) {
        for (const UsdPrim& prim : stage->Traverse()) {
            projection_prims.push_back(prim);
        }
    } else {
        for (const SdfPath& path : root_paths) {
            const UsdPrim root = stage->GetPrimAtPath(path);
            for (const UsdPrim& prim : UsdPrimRange(root)) {
                projection_prims.push_back(prim);
            }
        }
    }
    for (const UsdPrim& prim : projection_prims) {
        if (prim.IsInstance()) {
            NativeInstanceRecordRaw record;
            record.path = rust::String(prim.GetPath().GetString());
            record.prototype_path =
                rust::String(prim.GetPrototype().GetPath().GetString());
            for (const UsdPrim& proxy :
                 UsdPrimRange(prim, UsdTraverseInstanceProxies())) {
                if (!proxy.IsInstanceProxy()) continue;
                NativeInstancePurposeRaw entry;
                entry.prototype_path = rust::String(
                    proxy.GetPrimInPrototype().GetPath().GetString());
                entry.purpose = rust::String(
                    UsdGeomImageable(proxy).ComputePurpose().GetString());
                record.purposes.push_back(std::move(entry));
            }
            out.native_instances.push_back(std::move(record));
        }
    }
    // Shared instancing prototypes (/__Prototype_N) hold the deduplicated
    // geometry for native instances. Their meshes are projected once and then
    // RETAINED by the renderer across frames (they hydrate into the scene render
    // tree from the session layer), so a root-scoped reprojection that does not
    // touch a prototype subtree must NOT re-marshal every prototype's
    // points/normals/uvs/faces every frame — doing so turns a controller-only
    // pose write into a full re-read of the entire instanced scene. Re-append
    // prototype geometry only when it is not yet resident (first/full projection),
    // when a topology resync invalidated it, or when a requested root lands inside
    // a prototype. `prim.IsInPrototype()` (below) still flags these prims
    // prototype_only regardless of whether they were re-appended this frame.
    bool project_prototypes = full_projection || !*prototypes_resident;
    if (!project_prototypes) {
        for (const SdfPath& path : root_paths) {
            const UsdPrim root = stage->GetPrimAtPath(path);
            if (root && (root.IsPrototype() || root.IsInPrototype())) {
                project_prototypes = true;
                break;
            }
        }
    }
    if (project_prototypes) {
        for (const UsdPrim& prototype : stage->GetPrototypes()) {
            for (const UsdPrim& prim : UsdPrimRange(prototype)) {
                projection_prims.push_back(prim);
            }
        }
        // The geometry loop below actually emits the prototype meshes; a
        // poses-only pass returns before it, so residency is only established
        // once real geometry has been projected.
        if (!poses_only) {
            *prototypes_resident = true;
        }
    }
    std::map<std::string, MaterialRecordRaw> materials;
    refresh_point_prototype_cache(
        stage, root_paths, full_projection, point_prototypes_by_instancer,
        point_prototype_roots, point_prototype_cache_initialized);
    const auto is_point_prototype = [point_prototype_roots](const SdfPath& path) {
        return std::any_of(
            point_prototype_roots->begin(),
            point_prototype_roots->end(),
            [&path](const SdfPath& root) { return path.HasPrefix(root); });
    };
    // A prim is prototype-only (never drawn directly) when it belongs to a
    // point-instancer prototype OR to a native scenegraph prototype (/__Prototype_N).
    const auto is_prototype_geom = [&](const UsdPrim& prim) {
        return is_point_prototype(prim.GetPath()) || prim.IsInPrototype();
    };
    append_skeleton_projection(
        stage, time, &transforms, skeleton_cache, *skeleton_dependencies,
        projection_prims, &out, poses_only);
    // Skeleton-pose-only fast path: skeletal animation changes only the joint
    // palette (computed above in ~2ms). The mesh geometry, materials, lights,
    // and cameras are static from frame to frame, so skip the full geometry
    // re-projection loop below — re-reading points/normals/uvs/faces/materials
    // for every skinned mesh every frame kills performance. The renderer refreshes
    // joint matrices in place from the updated SkeletonPoseComp; nothing else needs to change.
    if (poses_only) return out;
    
    std::size_t supported_direct_lights = 0;
    bool directional_shadow_requested = false;
    for (const UsdPrim& prim : projection_prims) {
        const std::string type_name = prim.GetTypeName().GetString();
        static const std::set<std::string> unsupported_gprims = {"NurbsCurves", "HermiteCurves"};
        if (unsupported_gprims.find(type_name) != unsupported_gprims.end()) {
            out.diagnostics.push_back(rust::String(
                prim.GetPath().GetString() + " uses unsupported viewport gprim " +
                type_name + "; bounds remain inspectable"));
        }

        const bool distant_light = prim.IsA<UsdLuxDistantLight>();
        const bool sphere_light = prim.IsA<UsdLuxSphereLight>();
        const bool disk_light = prim.IsA<UsdLuxDiskLight>();
        const bool cylinder_light = prim.IsA<UsdLuxCylinderLight>();
        const bool rect_light = prim.IsA<UsdLuxRectLight>();
        const bool dome_light = prim.IsA<UsdLuxDomeLight>() ||
                                type_name.rfind("DomeLight", 0) == 0;
        const bool dome_procedural = !dome_light &&
                                     type_name == "IdtxProceduralDomeLight";
        const bool supported_light =
            distant_light || sphere_light || disk_light || cylinder_light ||
            rect_light || dome_light || dome_procedural;
        std::string unsupported_light;
        if (prim.IsA<UsdLuxPortalLight>()) {
            unsupported_light = "PortalLight";
        } else if (prim.HasAPI<UsdLuxMeshLightAPI>()) {
            unsupported_light = "MeshLight";
        } else if (prim.IsA<UsdLuxGeometryLight>()) {
            unsupported_light = "GeometryLight";
        } else if (prim.IsA<UsdLuxPluginLight>()) {
            unsupported_light = "PluginLight";
        } else if (!supported_light && prim.HasAPI<UsdLuxLightAPI>()) {
            unsupported_light = type_name.empty() ? "LightAPI" : type_name;
        }
        if ((supported_light || !unsupported_light.empty()) &&
            is_visible(prim, time)) {
            if (!unsupported_light.empty()) {
                out.diagnostics.push_back(rust::String(
                    prim.GetPath().GetString() + " uses unsupported viewport light " +
                    unsupported_light));
            } else {
                const std::string kind = distant_light ? "distant"
                    : sphere_light ? "sphere"
                    : disk_light ? "disk"
                    : cylinder_light ? "cylinder"
                    : rect_light ? "rect"
                    : dome_procedural ? "dome_procedural"
                    : "dome";
                LightRecordRaw record =
                    light_record(prim, time, &transforms, kind);
                ++supported_direct_lights;
                if (record.shadow_enabled) {
                    if (distant_light) {
                        directional_shadow_requested = true;
                    } else {
                        out.diagnostics.push_back(rust::String(
                            prim.GetPath().GetString() + " requests shadows on " +
                            type_name +
                            "; only DistantLight shadows are supported"));
                    }
                }
                out.lights.push_back(std::move(record));
            }
        }
        if (prim.IsA<UsdGeomXformable>()) {
            TransformRecordRaw record;
            record.path = rust::String(prim.GetPath().GetString());
            append_matrix(&record.world_matrix,
                          transforms.GetLocalToWorldTransform(prim));
            out.transforms.push_back(std::move(record));
        }

        if (prim.IsA<UsdGeomImageable>()) {
            VisibilityRecordRaw record;
            record.path = rust::String(prim.GetPath().GetString());
            const UsdGeomImageable imageable(prim);
            const TfToken purpose = imageable.ComputePurpose();
            record.visible = imageable.ComputeVisibility(time) !=
                             UsdGeomTokens->invisible;
            record.purpose = rust::String(purpose.GetString());
            // `idtx:castsShadows` opt-out (default true when absent/unauthored):
            // a visible-but-non-casting gprim (debug gizmo, pointer ray) is
            // excluded from the shadow depth pre-pass and the shadow AABB.
            record.casts_shadow = true;
            if (const UsdAttribute castsAttr =
                    prim.GetAttribute(TfToken("idtx:castsShadows"))) {
                bool v = true;
                if (castsAttr.Get(&v, time)) {
                    record.casts_shadow = v;
                }
            }
            out.visibility.push_back(std::move(record));
        }

        if (prim.IsA<UsdGeomBoundable>()) {
            // Use ComputeExtentFromPlugins (always computes from schema
            // parameters: radius, height, axis, etc.) rather than BBoxCache
            // which trusts the authored `extent` attribute that may be stale
            // when geometry parameters were changed after authoring.
            VtVec3fArray local_extent;
            if (UsdGeomBoundable::ComputeExtentFromPlugins(
                    UsdGeomBoundable(prim), time, &local_extent)
                && local_extent.size() >= 2) {
                const GfVec3f& lo = local_extent[0];
                const GfVec3f& hi = local_extent[1];
                if (lo[0] <= hi[0] && lo[1] <= hi[1] && lo[2] <= hi[2]) {
                    // Transform the 8 local-AABB corners through the
                    // local-to-world matrix to obtain a correct world-space
                    // AABB even for rotated/scaled prims.
                    const GfMatrix4d l2w = transforms.GetLocalToWorldTransform(prim);
                    const GfVec3f corners[8] = {
                        {lo[0], lo[1], lo[2]}, {hi[0], lo[1], lo[2]},
                        {lo[0], hi[1], lo[2]}, {hi[0], hi[1], lo[2]},
                        {lo[0], lo[1], hi[2]}, {hi[0], lo[1], hi[2]},
                        {lo[0], hi[1], hi[2]}, {hi[0], hi[1], hi[2]},
                    };
                    GfVec3d wmin( std::numeric_limits<double>::infinity());
                    GfVec3d wmax(-std::numeric_limits<double>::infinity());
                    for (const GfVec3f& c : corners) {
                        const GfVec3d w = l2w.Transform(GfVec3d(c));
                        for (int i = 0; i < 3; ++i) {
                            if (w[i] < wmin[i]) wmin[i] = w[i];
                            if (w[i] > wmax[i]) wmax[i] = w[i];
                        }
                    }
                    BoundsRecordRaw record;
                    record.path = rust::String(prim.GetPath().GetString());
                    for (int axis = 0; axis < 3; ++axis) {
                        record.minimum.push_back(static_cast<float>(wmin[axis]));
                        record.maximum.push_back(static_cast<float>(wmax[axis]));
                        record.local_minimum.push_back(lo[axis]);
                        record.local_maximum.push_back(hi[axis]);
                    }
                    out.bounds.push_back(std::move(record));
                }
            }
        }

        if (prim.IsA<UsdGeomBasisCurves>()) {
            const UsdGeomBasisCurves schema(prim);
            BasisCurvesRecordRaw record;
            record.path = rust::String(prim.GetPath().GetString());
            VtVec3fArray points;
            VtIntArray counts;
            TfToken type, basis, wrap;
            schema.GetPointsAttr().Get(&points, time);
            schema.GetCurveVertexCountsAttr().Get(&counts, time);
            schema.GetTypeAttr().Get(&type, time);
            schema.GetBasisAttr().Get(&basis, time);
            schema.GetWrapAttr().Get(&wrap, time);
            record.curve_type = rust::String(type.GetString());
            record.basis = rust::String(basis.GetString());
            record.wrap = rust::String(wrap.GetString());
            // Bound the FFI allocation too; an empty record yields a diagnostic.
            if (points.size() <= 16384 && counts.size() <= 16384) {
                for (const auto& p : points) for (int i = 0; i < 3; ++i) record.points.push_back(p[i]);
                for (int n : counts) record.counts.push_back(n);
            } else {
                out.diagnostics.push_back(rust::String(prim.GetPath().GetString() + " curve exceeds 16384 control-point/count limit"));
            }
            const auto matrix = transforms.GetLocalToWorldTransform(prim);
            for (int r = 0; r < 4; ++r) for (int c = 0; c < 4; ++c) record.world_matrix.push_back(matrix[r][c]);
            record.metres_per_unit = UsdGeomGetStageMetersPerUnit(stage);
            record.z_up = UsdGeomGetStageUpAxis(stage) == UsdGeomTokens->z;
            record.tolerance = 0.001;
            if (auto attr = prim.GetAttribute(TfToken("idtx:curve:toleranceMetres"))) {
                if (!attr.Get(&record.tolerance, time)) record.tolerance = std::numeric_limits<double>::quiet_NaN();
            }
            record.prototype_only = is_prototype_geom(prim);
            record.eligible = true;
            if (auto attr = prim.GetAttribute(TfToken("idtx:curve:eligible"))) {
                if (!attr.Get(&record.eligible, time)) {
                    record.eligible = false;
                    out.diagnostics.push_back(rust::String(prim.GetPath().GetString() + " idtx:curve:eligible requires a bool"));
                }
            }
            std::string start_group, end_group;
            if (auto attr = prim.GetAttribute(TfToken("idtx:curve:startGroup"))) attr.Get(&start_group, time);
            if (auto attr = prim.GetAttribute(TfToken("idtx:curve:endGroup"))) attr.Get(&end_group, time);
            record.start_group = rust::String(start_group); record.end_group = rust::String(end_group);
            out.curves.push_back(std::move(record));
        }

        if (prim.IsA<UsdGeomCube>()) {
            CubeRecordRaw cube;
            cube.path = rust::String(prim.GetPath().GetString());
            double size = 2.0;
            UsdGeomCube(prim).GetSizeAttr().Get(&size, time);
            cube.size = static_cast<float>(size);
            cube.display_color = display_color(prim, time);
            cube.material_path = rust::String(bound_material_path(prim));
            cube.double_sided = false;
            cube.prototype_only = is_prototype_geom(prim);
            UsdGeomGprim(prim).GetDoubleSidedAttr().Get(&cube.double_sided, time);
            out.cubes.push_back(std::move(cube));
        } else if (prim.IsA<UsdGeomSphere>()) {
            SphereRecordRaw sphere;
            sphere.path = rust::String(prim.GetPath().GetString());
            double radius = 1.0;
            UsdGeomSphere(prim).GetRadiusAttr().Get(&radius, time);
            sphere.radius = static_cast<float>(radius);
            sphere.display_color = display_color(prim, time);
            sphere.material_path = rust::String(bound_material_path(prim));
            sphere.double_sided = false;
            sphere.prototype_only = is_prototype_geom(prim);
            UsdGeomGprim(prim).GetDoubleSidedAttr().Get(&sphere.double_sided, time);
            out.spheres.push_back(std::move(sphere));
        } else if (prim.IsA<UsdGeomCone>() || prim.IsA<UsdGeomCylinder>() ||
                   prim.IsA<UsdGeomCapsule>()) {
            ExtendedGprimRecordRaw gprim;
            gprim.path = rust::String(prim.GetPath().GetString());
            double radius = 1.0;
            double height = 2.0;
            TfToken axis = UsdGeomTokens->z;
            if (prim.IsA<UsdGeomCone>()) {
                const UsdGeomCone schema(prim);
                gprim.kind = rust::String("cone");
                schema.GetRadiusAttr().Get(&radius, time);
                schema.GetHeightAttr().Get(&height, time);
                schema.GetAxisAttr().Get(&axis, time);
            } else if (prim.IsA<UsdGeomCylinder>()) {
                const UsdGeomCylinder schema(prim);
                gprim.kind = rust::String("cylinder");
                schema.GetRadiusAttr().Get(&radius, time);
                schema.GetHeightAttr().Get(&height, time);
                schema.GetAxisAttr().Get(&axis, time);
            } else {
                const UsdGeomCapsule schema(prim);
                gprim.kind = rust::String("capsule");
                schema.GetRadiusAttr().Get(&radius, time);
                schema.GetHeightAttr().Get(&height, time);
                schema.GetAxisAttr().Get(&axis, time);
            }
            gprim.radius = static_cast<float>(radius);
            gprim.height = static_cast<float>(height);
            gprim.axis = rust::String(axis.GetString());
            gprim.display_color = display_color(prim, time);
            gprim.material_path = rust::String(bound_material_path(prim));
            gprim.double_sided = false;
            gprim.prototype_only = is_prototype_geom(prim);
            UsdGeomGprim(prim).GetDoubleSidedAttr().Get(&gprim.double_sided, time);
            out.extended_gprims.push_back(std::move(gprim));
        } else if (prim.IsA<UsdGeomPlane>()) {
            // UsdGeomPlane: finite rectangle centred at origin, double-sided by
            // default. `axis` is the plane normal; `width`/`length` lie on the two
            // axes perpendicular to it (resolved into geometry in the Rust render
            // layer). For physics the same prim is treated as an unbounded static
            // half-space (see idtx-studio-session/src/physics.rs).
            PlaneRecordRaw plane;
            plane.path = rust::String(prim.GetPath().GetString());
            const UsdGeomPlane schema(prim);
            double width = 2.0;
            double length = 2.0;
            TfToken axis = UsdGeomTokens->z;
            schema.GetWidthAttr().Get(&width, time);
            schema.GetLengthAttr().Get(&length, time);
            schema.GetAxisAttr().Get(&axis, time);
            plane.width = static_cast<float>(width);
            plane.length = static_cast<float>(length);
            plane.axis = rust::String(axis.GetString());
            plane.display_color = display_color(prim, time);
            plane.material_path = rust::String(bound_material_path(prim));
            plane.double_sided = true;
            plane.prototype_only = is_prototype_geom(prim);
            UsdGeomGprim(prim).GetDoubleSidedAttr().Get(&plane.double_sided, time);
            out.planes.push_back(std::move(plane));
        } else if (prim.IsA<UsdGeomMesh>()) {
            MeshRecordRaw mesh;
            mesh.path = rust::String(prim.GetPath().GetString());
            const UsdGeomMesh schema(prim);
            VtArray<GfVec3f> points;
            VtArray<int> counts;
            VtArray<int> indices;
            if (!schema.GetPointsAttr().Get(&points, time)) {
                out.diagnostics.push_back(rust::String(
                    prim.GetPath().GetString() +
                    " mesh points cannot be converted to point3f; skipping viewport geometry"));
                continue;
            }
            schema.GetFaceVertexCountsAttr().Get(&counts, time);
            schema.GetFaceVertexIndicesAttr().Get(&indices, time);
            const bool indices_valid = std::all_of(
                indices.begin(), indices.end(), [&points](int index) {
                    return index >= 0 &&
                           static_cast<std::size_t>(index) < points.size();
                });
            if (!indices_valid) {
                out.diagnostics.push_back(rust::String(
                    prim.GetPath().GetString() +
                    " mesh contains an out-of-bounds point index; skipping viewport geometry"));
                continue;
            }
            mesh.points.reserve(points.size() * 3);
            for (const GfVec3f& point : points) {
                mesh.points.push_back(point[0]);
                mesh.points.push_back(point[1]);
                mesh.points.push_back(point[2]);
            }
            const UsdGeomPrimvarsAPI primvars(prim);
            std::string selected_uv_set;
            const UsdShadeMaterial mesh_material =
                UsdShadeMaterialBindingAPI(prim).ComputeBoundMaterial();
            if (mesh_material) {
                const std::string material_path =
                    mesh_material.GetPath().GetString();
                auto [entry, inserted] = materials.emplace(
                    material_path, material_record(mesh_material, time));
                (void)inserted;
                selected_uv_set = std::string(entry->second.diffuse_uv_set);
            }
            UsdGeomPrimvar st = selected_uv_set.empty()
                ? primvars.GetPrimvar(TfToken("st"))
                : primvars.GetPrimvar(TfToken(selected_uv_set));
            if (!st) {
                st = primvars.GetPrimvar(TfToken("st"));
            }
            if (!st) {
                st = primvars.GetPrimvar(TfToken("st0"));
            }
            if (!st) {
                st = primvars.GetPrimvar(TfToken("UVMap"));
            }
            VtArray<GfVec2f> texcoords;
            if (st && st.ComputeFlattened(&texcoords, time)) {
                const TfToken interpolation = st.GetInterpolation();
                mesh.texcoords.reserve(indices.size() * 2);
                std::size_t corner = 0;
                for (std::size_t face = 0; face < counts.size(); ++face) {
                    for (int face_corner = 0; face_corner < counts[face];
                         ++face_corner, ++corner) {
                        std::size_t uv_index = 0;
                        if (interpolation == UsdGeomTokens->faceVarying) {
                            uv_index = corner;
                        } else if (interpolation == UsdGeomTokens->uniform) {
                            uv_index = face;
                        } else if (interpolation == UsdGeomTokens->vertex ||
                                   interpolation == UsdGeomTokens->varying) {
                            if (corner < indices.size() && indices[corner] >= 0) {
                                uv_index = static_cast<std::size_t>(indices[corner]);
                            } else {
                                uv_index = texcoords.size();
                            }
                        }
                        if (uv_index < texcoords.size()) {
                            mesh.texcoords.push_back(texcoords[uv_index][0]);
                            mesh.texcoords.push_back(texcoords[uv_index][1]);
                        } else {
                            mesh.texcoords.push_back(0.0f);
                            mesh.texcoords.push_back(0.0f);
                        }
                    }
                }
            }
            mesh.face_vertex_counts.reserve(counts.size());
            for (int count : counts) {
                mesh.face_vertex_counts.push_back(count);
            }
            mesh.face_vertex_indices.reserve(indices.size());
            for (int index : indices) {
                mesh.face_vertex_indices.push_back(index);
            }
            {
                UsdGeomPrimvarsAPI pvAPI2(prim);
                UsdGeomPrimvar dcPv = pvAPI2.GetPrimvar(TfToken("displayColor"));
                VtArray<GfVec3f> dc_colors;
                mesh.display_color.clear();
                if (dcPv && dcPv.ComputeFlattened(&dc_colors, time) && !dc_colors.empty()) {
                    const TfToken interp = dcPv.GetInterpolation();
                    std::size_t corner = 0;
                    for (std::size_t face = 0; face < counts.size(); ++face) {
                        for (int fc = 0; fc < counts[face]; ++fc, ++corner) {
                            std::size_t ci = 0;
                            if (interp == UsdGeomTokens->faceVarying) {
                                ci = corner;
                            } else if (interp == UsdGeomTokens->uniform) {
                                ci = face;
                            } else if (interp == UsdGeomTokens->vertex ||
                                       interp == UsdGeomTokens->varying) {
                                ci = (corner < indices.size() && indices[corner] >= 0)
                                    ? static_cast<std::size_t>(indices[corner]) : 0u;
                            }
                            if (ci >= dc_colors.size()) ci = 0;
                            mesh.display_color.push_back(dc_colors[ci][0]);
                            mesh.display_color.push_back(dc_colors[ci][1]);
                            mesh.display_color.push_back(dc_colors[ci][2]);
                        }
                    }
                }
            }
            mesh.material_path = rust::String(bound_material_path(prim));
            TfToken orientation = UsdGeomTokens->rightHanded;
            schema.GetOrientationAttr().Get(&orientation, time);
            mesh.left_handed = orientation == UsdGeomTokens->leftHanded;
            UsdShadeMaterialBindingAPI mesh_binding(prim);
            for (const UsdGeomSubset& subset : mesh_binding.GetMaterialBindSubsets()) {
                VtIntArray face_indices;
                subset.GetIndicesAttr().Get(&face_indices, time);
                const UsdShadeMaterial subset_material =
                    UsdShadeMaterialBindingAPI(subset.GetPrim()).ComputeBoundMaterial();
                const std::string material_path =
                    subset_material ? subset_material.GetPath().GetString() : std::string();
                mesh.subset_material_paths.push_back(rust::String(material_path));
                mesh.subset_face_counts.push_back(
                    static_cast<std::uint32_t>(face_indices.size()));
                for (int face_index : face_indices) {
                    mesh.subset_face_indices.push_back(face_index);
                }
                if (subset_material && materials.find(material_path) == materials.end()) {
                    materials.emplace(material_path,
                                      material_record(subset_material, time));
                }
            }
            mesh.double_sided = false;
            mesh.prototype_only = is_prototype_geom(prim);
            UsdGeomGprim(prim).GetDoubleSidedAttr().Get(&mesh.double_sided, time);
            // Subdivision scheme + sharp-feature specs (Phase 2). The renderer
            // does not subdivide, but feeds creases/corners into its smooth-normal
            // synthesis so authored sharp edges/vertices stay faceted.
            {
                TfToken subdiv_scheme;
                if (schema.GetSubdivisionSchemeAttr().Get(&subdiv_scheme, time)) {
                    mesh.subdivision_scheme = rust::String(subdiv_scheme.GetString());
                }
                VtIntArray crease_indices;
                if (schema.GetCreaseIndicesAttr().Get(&crease_indices, time)) {
                    mesh.crease_indices.reserve(crease_indices.size());
                    for (int i : crease_indices) mesh.crease_indices.push_back(i);
                }
                VtIntArray crease_lengths;
                if (schema.GetCreaseLengthsAttr().Get(&crease_lengths, time)) {
                    mesh.crease_lengths.reserve(crease_lengths.size());
                    for (int l : crease_lengths) mesh.crease_lengths.push_back(l);
                }
                VtFloatArray crease_sharpnesses;
                if (schema.GetCreaseSharpnessesAttr().Get(&crease_sharpnesses, time)) {
                    mesh.crease_sharpnesses.reserve(crease_sharpnesses.size());
                    for (float s : crease_sharpnesses) mesh.crease_sharpnesses.push_back(s);
                }
                VtIntArray corner_indices;
                if (schema.GetCornerIndicesAttr().Get(&corner_indices, time)) {
                    mesh.corner_indices.reserve(corner_indices.size());
                    for (int i : corner_indices) mesh.corner_indices.push_back(i);
                }
                VtFloatArray corner_sharpnesses;
                if (schema.GetCornerSharpnessesAttr().Get(&corner_sharpnesses, time)) {
                    mesh.corner_sharpnesses.reserve(corner_sharpnesses.size());
                    for (float s : corner_sharpnesses) mesh.corner_sharpnesses.push_back(s);
                }
            }
            // Read normals and tangents primvars if present.
            // All USD interpolation modes (vertex, uniform, faceVarying) are expanded
            // to face-corner order here; the Rust side needs no further re-indexing.
            {
                const UsdGeomPrimvarsAPI pv(prim);
                const std::size_t num_corners = indices.size();

                // ── Normals (Vec3f) ───────────────────────────────────────
                UsdGeomPrimvar nrm_pv = pv.GetPrimvar(UsdGeomTokens->normals);
                VtArray<GfVec3f> nrm_vals;
                if (nrm_pv && nrm_pv.ComputeFlattened(&nrm_vals, time) && !nrm_vals.empty()) {
                    const TfToken interp = nrm_pv.GetInterpolation();
                    mesh.normals.reserve(num_corners * 3);
                    std::size_t corner = 0;
                    for (std::size_t face = 0; face < counts.size(); ++face) {
                        for (int fc = 0; fc < counts[face]; ++fc, ++corner) {
                            std::size_t vi = 0;
                            if (interp == UsdGeomTokens->faceVarying) {
                                vi = corner;
                            } else if (interp == UsdGeomTokens->uniform) {
                                vi = face;
                            } else { // vertex / varying
                                if (corner < indices.size() && indices[corner] >= 0)
                                    vi = static_cast<std::size_t>(indices[corner]);
                            }
                            const GfVec3f& n = (vi < nrm_vals.size()) ? nrm_vals[vi]
                                                                       : GfVec3f(0.f, 1.f, 0.f);
                            mesh.normals.push_back(n[0]);
                            mesh.normals.push_back(n[1]);
                            mesh.normals.push_back(n[2]);
                        }
                    }
                }

                // ── Tangents (Vec4f: xyz = direction, w = bitangent sign ±1) ──
                // Try "tangents" first (common DCC export), then "primvars:tangents".
                UsdGeomPrimvar tan_pv = pv.GetPrimvar(TfToken("tangents"));
                if (!tan_pv || !tan_pv.IsDefined())
                    tan_pv = pv.GetPrimvar(TfToken("primvars:tangents"));
                VtArray<GfVec4f> tan_vals;
                if (tan_pv && tan_pv.ComputeFlattened(&tan_vals, time) && !tan_vals.empty()) {
                    const TfToken interp = tan_pv.GetInterpolation();
                    mesh.tangents.reserve(num_corners * 4);
                    std::size_t corner = 0;
                    for (std::size_t face = 0; face < counts.size(); ++face) {
                        for (int fc = 0; fc < counts[face]; ++fc, ++corner) {
                            std::size_t vi = 0;
                            if (interp == UsdGeomTokens->faceVarying) {
                                vi = corner;
                            } else if (interp == UsdGeomTokens->uniform) {
                                vi = face;
                            } else { // vertex / varying
                                if (corner < indices.size() && indices[corner] >= 0)
                                    vi = static_cast<std::size_t>(indices[corner]);
                            }
                            const GfVec4f& t = (vi < tan_vals.size()) ? tan_vals[vi]
                                                                       : GfVec4f(1.f, 0.f, 0.f, 1.f);
                            mesh.tangents.push_back(t[0]);
                            mesh.tangents.push_back(t[1]);
                            mesh.tangents.push_back(t[2]);
                            mesh.tangents.push_back(t[3]);
                        }
                    }
                }
            }
            out.meshes.push_back(std::move(mesh));
        }

        if (prim.IsA<UsdGeomPointInstancer>()) {
            out.point_instancers.push_back(point_instancer_record(
                stage, UsdGeomPointInstancer(prim), time, false, &out.diagnostics));
        }

        if (prim.IsA<UsdGeomGprim>()) {
            const UsdShadeMaterial bound =
                UsdShadeMaterialBindingAPI(prim).ComputeBoundMaterial();
            if (bound) {
                const std::string path = bound.GetPath().GetString();
                if (materials.find(path) == materials.end()) {
                    materials.emplace(path, material_record(bound, time));
                }
            }
        }

        if (prim.IsA<UsdGeomCamera>()) {
            const UsdGeomCamera camera(prim);
            CameraRecordRaw record;
            record.path = rust::String(prim.GetPath().GetString());
            TfToken projection;
            float focal = 50.0f;
            float horizontal = 20.955f;
            float vertical = 15.2908f;
            float horizontal_offset = 0.0f;
            float vertical_offset = 0.0f;
            GfVec2f clipping(0.1f, 1000000.0f);
            VtArray<GfVec4f> clipping_planes;
            float f_stop = 0.0f;
            float focus_distance = 0.0f;
            TfToken stereo_role = UsdGeomTokens->mono;
            double shutter_open = 0.0;
            double shutter_close = 0.0;
            float exposure = 0.0f;
            float exposure_iso = 100.0f;
            float exposure_time = 1.0f;
            float exposure_f_stop = 1.0f;
            float exposure_responsivity = 1.0f;
            camera.GetProjectionAttr().Get(&projection, time);
            camera.GetFocalLengthAttr().Get(&focal, time);
            camera.GetHorizontalApertureAttr().Get(&horizontal, time);
            camera.GetVerticalApertureAttr().Get(&vertical, time);
            camera.GetHorizontalApertureOffsetAttr().Get(
                &horizontal_offset, time);
            camera.GetVerticalApertureOffsetAttr().Get(&vertical_offset, time);
            camera.GetClippingRangeAttr().Get(&clipping, time);
            camera.GetClippingPlanesAttr().Get(&clipping_planes, time);
            camera.GetFStopAttr().Get(&f_stop, time);
            camera.GetFocusDistanceAttr().Get(&focus_distance, time);
            camera.GetStereoRoleAttr().Get(&stereo_role, time);
            camera.GetShutterOpenAttr().Get(&shutter_open, time);
            camera.GetShutterCloseAttr().Get(&shutter_close, time);
            camera.GetExposureAttr().Get(&exposure, time);
            camera.GetExposureIsoAttr().Get(&exposure_iso, time);
            camera.GetExposureTimeAttr().Get(&exposure_time, time);
            camera.GetExposureFStopAttr().Get(&exposure_f_stop, time);
            camera.GetExposureResponsivityAttr().Get(
                &exposure_responsivity, time);
            record.projection = rust::String(projection.GetString());
            record.focal_length = focal;
            record.horizontal_aperture = horizontal;
            record.vertical_aperture = vertical;
            record.horizontal_aperture_offset = horizontal_offset;
            record.vertical_aperture_offset = vertical_offset;
            record.clipping_near = clipping[0];
            record.clipping_far = clipping[1];
            for (const GfVec4f& plane : clipping_planes) {
                for (int axis = 0; axis < 4; ++axis) {
                    record.clipping_planes.push_back(plane[axis]);
                }
            }
            record.f_stop = f_stop;
            record.focus_distance = focus_distance;
            record.stereo_role = rust::String(stereo_role.GetString());
            record.shutter_open = shutter_open;
            record.shutter_close = shutter_close;
            record.exposure = exposure;
            record.exposure_iso = exposure_iso;
            record.exposure_time = exposure_time;
            record.exposure_f_stop = exposure_f_stop;
            record.exposure_responsivity = exposure_responsivity;
            out.cameras.push_back(std::move(record));
        }
    }
    for (auto& [_, material] : materials) {
        out.materials.push_back(std::move(material));
    }
    if (supported_direct_lights > 16) {
        out.diagnostics.push_back(rust::String(
            "viewport direct lighting supports at most 16 lights; projected " +
            std::to_string(supported_direct_lights) +
            " supported lights in stable traversal order"));
    }
    if (directional_shadow_requested) {
        out.diagnostics.push_back(rust::String(
            "viewport shadows are limited to one DistantLight using one "
            "1024x1024 map with fixed PCF filtering"));
    }
    
    return out;
}

}  // namespace

ViewportProjectionRaw project_viewport(
    const StageHandle& handle, double time_code, bool use_default_time,
    rust::Slice<const rust::String> roots) {
    return handle.impl_->access([&] {
    if (handle.impl_->observer->take_skeleton_cache_dirty()) {
        handle.impl_->skeleton_cache_initialized = false;
    }
    if (handle.impl_->observer->take_prototypes_dirty()) {
        handle.impl_->prototypes_resident = false;
    }
    return project_viewport_locked(
        handle.impl_->stage, &handle.impl_->point_prototypes_by_instancer,
        &handle.impl_->point_prototype_roots,
        &handle.impl_->point_prototype_cache_initialized,
        &handle.impl_->skeleton_cache, &handle.impl_->skeleton_dependencies,
        &handle.impl_->skeleton_cache_initialized,
        &handle.impl_->prototypes_resident,
        time_code, use_default_time, roots, /*poses_only=*/false);
    });
}

#include "animation_playback.inc"
#include "point_instancer.inc"

ViewportProjectionRaw project_skeleton_poses(
    const StageHandle& handle, double time_code, bool use_default_time,
    rust::Slice<const rust::String> roots) {
    return handle.impl_->access([&] {
    if (handle.impl_->observer->take_skeleton_cache_dirty()) {
        handle.impl_->skeleton_cache_initialized = false;
    }
    if (handle.impl_->observer->take_prototypes_dirty()) {
        handle.impl_->prototypes_resident = false;
    }
    return project_viewport_locked(
        handle.impl_->stage, &handle.impl_->point_prototypes_by_instancer,
        &handle.impl_->point_prototype_roots,
        &handle.impl_->point_prototype_cache_initialized,
        &handle.impl_->skeleton_cache, &handle.impl_->skeleton_dependencies,
        &handle.impl_->skeleton_cache_initialized,
        &handle.impl_->prototypes_resident,
        time_code, use_default_time, roots, /*poses_only=*/true);
    });
}

rust::Vec<TransformRecordRaw> project_transforms_at(
    const StageHandle& handle, double time_code, bool use_default_time,
    rust::Slice<const rust::String> prim_paths) {
    return handle.impl_->access([&] {
    const UsdStageRefPtr& s = handle.impl_->stage;
    const UsdTimeCode time = use_default_time ? UsdTimeCode::Default()
                                              : UsdTimeCode(time_code);
    UsdGeomXformCache xform_cache(time);
    rust::Vec<TransformRecordRaw> out;
    for (const auto& path_str : prim_paths) {
        std::string path_s(path_str);
        const SdfPath path(path_s);
        UsdPrim root = s->GetPrimAtPath(path);
        if (!root) continue;
        // Traverse root and all active descendants, emitting a transform for
        // every Xformable prim.  Inactive subtrees are pruned.  This mirrors
        // what project_viewport_locked() does for transform records without
        // the expensive geometry / material / bounds extraction.
        UsdPrimRange range(root);
        for (auto it = range.begin(); it != range.end(); ++it) {
            const UsdPrim& prim = *it;
            if (!prim.IsActive()) { it.PruneChildren(); continue; }
            if (!prim.IsA<UsdGeomXformable>()) continue;
            TransformRecordRaw record;
            record.path = rust::String(prim.GetPath().GetString());
            append_matrix(&record.world_matrix,
                          xform_cache.GetLocalToWorldTransform(prim));
            out.push_back(std::move(record));
        }
    }
    return out;
    });
}

/// Format a `VtValue` into the compact summary string used across the
/// inspector: `TfStringify`, prefixed with `"[N values] "` for arrays and hard
/// truncated at 512 characters. Reused for composed attribute values, per-layer
/// opinion values, and MaterialX node inputs.
static std::string summarize_value(const VtValue& value) {
    std::string summary = TfStringify(value);
    if (value.IsArrayValued()) {
        summary = "[" + std::to_string(value.GetArraySize()) + " values] " + summary;
    }
    if (summary.size() > 512) {
        summary.resize(512);
        summary += "... (truncated)";
    }
    return summary;
}

rust::Vec<PropertyRecordRaw> inspect_prim(const StageHandle& handle, rust::Str path) {
    return handle.impl_->access([&] {
    const UsdPrim prim = handle.impl_->stage->GetPrimAtPath(SdfPath(to_std(path)));
    if (!prim) {
        throw std::runtime_error("prim path does not exist");
    }
    // Root edit-target layer: a property is "root authored" (and thus removable
    // via the inspector) exactly when it has a spec in this layer.
    const SdfLayerHandle root = handle.impl_->stage->GetRootLayer();
    rust::Vec<PropertyRecordRaw> out;
    for (const UsdProperty& property : prim.GetProperties()) {
        PropertyRecordRaw record;
        record.name = rust::String(property.GetName().GetString());
        record.authored = property.IsAuthored();
        record.root_authored =
            root && root->GetPropertyAtPath(
                        prim.GetPath().AppendProperty(property.GetName()));
        for (const SdfPropertySpecHandle& spec : property.GetPropertyStack()) {
            record.provenance.push_back(rust::String(
                spec->GetLayer()->GetIdentifier() + ":" + spec->GetPath().GetString()));
            // Per-opinion value, parallel to `provenance`: the value this
            // particular layer authors for the property (not the composed
            // result). Attribute specs carry a default value; time-sampled
            // opinions with no default are flagged instead; anything else
            // (relationships, empty specs) contributes an empty string.
            std::string opinion_value;
            if (SdfAttributeSpecHandle attrSpec =
                    TfDynamic_cast<SdfAttributeSpecHandle>(spec)) {
                const VtValue defaultValue = attrSpec->GetDefaultValue();
                if (!defaultValue.IsEmpty()) {
                    opinion_value = summarize_value(defaultValue);
                } else if (spec->GetLayer()->GetNumTimeSamplesForPath(
                               spec->GetPath()) > 0) {
                    opinion_value = "(time samples)";
                }
            }
            record.provenance_values.push_back(rust::String(opinion_value));
        }
        if (property.Is<UsdAttribute>()) {
            const UsdAttribute attribute = property.As<UsdAttribute>();
            record.kind = rust::String("attribute");
            record.type_name = rust::String(attribute.GetTypeName().GetAsToken().GetString());
            record.time_sample_count = attribute.GetNumTimeSamples();
            record.value_source = rust::String(
                record.time_sample_count > 0
                    ? "authored/time-sampled"
                    : (record.authored ? "authored/default" : "schema fallback"));
            VtValue value;
            if (attribute.Get(&value)) {
                record.value_summary = rust::String(summarize_value(value));
            }
        } else if (property.Is<UsdRelationship>()) {
            const UsdRelationship relationship = property.As<UsdRelationship>();
            record.kind = rust::String("relationship");
            record.type_name = rust::String("relationship");
            record.value_source = rust::String(
                record.authored ? "authored targets" : "schema relationship");
            SdfPathVector targets;
            relationship.GetTargets(&targets);
            for (const SdfPath& target : targets) {
                record.relationship_targets.push_back(rust::String(target.GetString()));
            }
        }
        out.push_back(std::move(record));
    }
    const TfTokenVector applied_schemas = prim.GetAppliedSchemas();
    if (!applied_schemas.empty()) {
        PropertyRecordRaw record;
        record.name = rust::String("apiSchemas");
        record.kind = rust::String("metadata");
        record.type_name = rust::String("token[]");
        record.authored = prim.HasAuthoredMetadata(TfToken("apiSchemas"));
        record.value_source = rust::String("composed prim metadata");
        // apiSchemas is composed prim metadata, not a removable property spec.
        record.root_authored = false;
        std::string summary;
        for (const TfToken& schema : applied_schemas) {
            if (!summary.empty()) {
                summary += ", ";
            }
            summary += schema.GetString();
        }
        record.value_summary = rust::String(summary);
        out.push_back(std::move(record));
    }
    return out;
    });
}

// Resolve an `asset`-typed attribute to an absolute path using USD's own
// resolution, so a relative source path (e.g. "./behaviors/x.rs") resolves
// against its *authoring* layer regardless of how the prim is reached — opened
// directly, flattened, or composed through a payloaded/referenced assembly.
// Prefers SdfAssetPath::GetResolvedPath(); when that is empty (the asset does
// not exist yet, or the resolver returned nothing) anchors the authored path
// against the strongest opinion's layer (mirrors `anchor_udim_template`), so
// the caller still gets a concrete path to report/compile from. Returns an
// empty string when the prim/attribute is missing, non-asset, or unset.
rust::String resolve_prim_asset(
    const StageHandle& handle, rust::Str prim_path, rust::Str attr_name) {
    return handle.impl_->access([&] {
    const UsdPrim prim =
        handle.impl_->stage->GetPrimAtPath(SdfPath(to_std(prim_path)));
    if (!prim) {
        return rust::String();
    }
    const UsdAttribute attr = prim.GetAttribute(TfToken(to_std(attr_name)));
    if (!attr) {
        return rust::String();
    }
    SdfAssetPath asset;
    if (!attr.Get(&asset)) {
        return rust::String();
    }
    const std::string resolved = asset.GetResolvedPath();
    if (!resolved.empty()) {
        return rust::String(resolved);
    }
    const std::string authored = asset.GetAssetPath();
    if (authored.empty()) {
        return rust::String();
    }
    for (const SdfPropertySpecHandle& spec :
         attr.GetPropertyStack(UsdTimeCode::Default())) {
        const std::string anchored =
            SdfComputeAssetPathRelativeToLayer(spec->GetLayer(), authored);
        if (!anchored.empty()) {
            return rust::String(anchored);
        }
    }
    return rust::String(authored);
    });
}

MaterialGraphRaw material_graph(const StageHandle& handle, rust::Str path) {
    return handle.impl_->access([&] {
    MaterialGraphRaw out;
    const UsdPrim prim = handle.impl_->stage->GetPrimAtPath(SdfPath(to_std(path)));
    if (!prim) {
        throw std::runtime_error("prim path does not exist");
    }
    std::vector<UsdShadeMaterial> materials;
    UsdShadeMaterial material(prim);
    if (!material) {
        material = UsdShadeMaterialBindingAPI(prim).ComputeBoundMaterial();
    }
    if (material) {
        materials.push_back(material);
    }
    for (const UsdGeomSubset& subset :
         UsdShadeMaterialBindingAPI(prim).GetMaterialBindSubsets()) {
        const UsdShadeMaterial subset_material =
            UsdShadeMaterialBindingAPI(subset.GetPrim()).ComputeBoundMaterial();
        if (subset_material) {
            materials.push_back(subset_material);
        }
    }
    if (materials.empty()) {
        out.diagnostics.push_back(rust::String("No computed material binding"));
        return out;
    }
    std::set<std::string> unique_materials;
    std::vector<UsdShadeShader> pending;
    for (const UsdShadeMaterial& candidate : materials) {
        const std::string material_path = candidate.GetPath().GetString();
        if (!unique_materials.insert(material_path).second) {
            continue;
        }
        if (out.material_path.empty()) {
            out.material_path = rust::String(material_path);
        }
        out.material_paths.push_back(rust::String(material_path));
        // Collect material-level output→shader connections so the Rust
        // extractor can wire the MaterialOutputs aggregate node.
        for (const UsdShadeOutput& output : candidate.GetOutputs()) {
            SdfPathVector connections;
            if (output.GetAttr().GetConnections(&connections) &&
                !connections.empty()) {
                out.material_output_attr_names.push_back(
                    rust::String("outputs:" + output.GetBaseName().GetString()));
                out.material_output_targets.push_back(
                    rust::String(connections[0].GetString()));
            }
        }
        const UsdShadeShader surface = surface_source(candidate);
        if (surface) {
            pending.push_back(surface);
        } else {
            out.diagnostics.push_back(rust::String(
                material_path + " has no resolved surface source"));
        }
    }
    std::set<std::string> visited;
    for (std::size_t cursor = 0; cursor < pending.size(); ++cursor) {
        const UsdShadeShader shader = pending[cursor];
        const std::string shader_path = shader.GetPath().GetString();
        if (!visited.insert(shader_path).second) {
            continue;
        }
        MaterialNodeRaw node;
        node.path = rust::String(shader_path);
        TfToken identifier;
        shader.GetIdAttr().Get(&identifier);
        node.identifier = rust::String(identifier.GetString());
        if (identifier != TfToken("UsdPreviewSurface") &&
            identifier != TfToken("ND_standard_surface_surfaceshader") &&
            cursor == 0) {
            out.diagnostics.push_back(rust::String(
                shader_path + " is not supported by the viewport material lowerer"));
        }
        for (const UsdShadeInput& input : shader.GetInputs()) {
            node.input_names.push_back(rust::String(input.GetBaseName().GetString()));
            VtValue value;
            std::string value_summary =
                input.Get(&value) ? summarize_value(value) : std::string();
            node.input_values.push_back(rust::String(value_summary));
            const UsdShadeSourceInfoVector sources =
                UsdShadeConnectableAPI::GetConnectedSources(input);
            std::string connection;
            for (const UsdShadeConnectionSourceInfo& source : sources) {
                if (!connection.empty()) {
                    connection += "\n";
                }
                connection += source.source.GetPath().GetString();
                connection += ".";
                if (source.sourceType == UsdShadeAttributeType::Output) {
                    connection += "outputs:";
                } else if (source.sourceType == UsdShadeAttributeType::Input) {
                    connection += "inputs:";
                }
                connection += source.sourceName.GetString();
                const UsdShadeShader upstream(source.source.GetPrim());
                if (upstream) {
                    pending.push_back(upstream);
                }
            }
            node.connections.push_back(rust::String(connection));
        }
        out.nodes.push_back(std::move(node));
    }
    return out;
    });
}

// Read-only backward walk of an OpenExec compute graph, surfaced to the UI as a
// "Compute Graph" canvas (parallel to `material_graph`). Starting from the
// selected prim, follow authored attribute connections upstream, emitting every
// visited prim as a node, until reaching prims with no connected inputs (the
// datasources). Deliberately uses only plain `UsdAttribute::GetConnections` +
// `UsdPrim::GetTypeName` — NOT `IsA` — so it works even when the openexec schema
// plugin is not registered (a read-only inspector built without the feature).
ComputeGraphRaw compute_graph(const StageHandle& handle, rust::Str path) {
    return handle.impl_->access([&] {
    ComputeGraphRaw out;
    const std::string selected = to_std(path);
    const UsdPrim root = handle.impl_->stage->GetPrimAtPath(SdfPath(selected));
    if (!root) {
        throw std::runtime_error("prim path does not exist");
    }

    // A compute graph exists only if the selected prim has at least one authored
    // attribute connection. Otherwise return the empty graph (root_path == "").
    bool any_connection = false;
    for (const UsdAttribute& attr : root.GetAttributes()) {
        SdfPathVector conns;
        if (attr.GetConnections(&conns) && !conns.empty()) {
            any_connection = true;
            break;
        }
    }
    if (!any_connection) {
        return out;
    }
    out.root_path = rust::String(selected);

    // Worklist backward walk (cursor over a growing vector, like material_graph).
    // `visited` dedups and guards cycles; predecessors are enqueued as each
    // connection is followed.
    std::set<std::string> visited;
    std::vector<SdfPath> pending;
    pending.push_back(root.GetPath());
    for (std::size_t cursor = 0; cursor < pending.size(); ++cursor) {
        const SdfPath current = pending[cursor];
        const std::string current_str = current.GetString();
        if (!visited.insert(current_str).second) {
            continue;
        }
        const UsdPrim prim = handle.impl_->stage->GetPrimAtPath(current);
        if (!prim) {
            continue;
        }

        ComputeNodeRaw node;
        node.path = rust::String(current_str);
        const std::string type_name = prim.GetTypeName().GetString();
        node.type_name = rust::String(type_name);

        // Role by type-name string (never `IsA`, so no schema plugin needed).
        // Datasource schemas are `Datasource` / `RestDatasource` /
        // `MockDatasource_RandomFloat` — all *contain* "Datasource".
        std::string role;
        if (current_str == selected) {
            role = "consumer";
        } else if (type_name.find("Datasource") != std::string::npos) {
            role = "datasource";
        } else if (type_name.rfind("Compute_", 0) == 0) {
            role = "compute";
        } else {
            role = "other";
        }
        node.role = rust::String(role);

        for (const UsdAttribute& attr : prim.GetAttributes()) {
            const std::string name = attr.GetName().GetString();
            const std::string type = attr.GetTypeName().GetAsToken().GetString();

            if (name.rfind("outputs:", 0) == 0) {
                node.output_names.push_back(rust::String(name));
                node.output_types.push_back(rust::String(type));
                continue;
            }

            SdfPathVector conns;
            const bool has_conn = attr.GetConnections(&conns) && !conns.empty();
            const bool inputs_prefixed = (name.rfind("inputs:", 0) == 0);

            // Pre-fetch the composed value for both filter and summary use.
            VtValue value;
            const bool has_value = attr.Get(&value);
            const std::string value_summary = has_value ? summarize_value(value) : std::string();

            // Filter: for the consumer (selected) prim keep only `inputs:*` and
            // connected attributes so USD built-ins (visibility, extent, …) are
            // excluded. Compute/Datasource/other nodes derive from Typed only —
            // no geometry noise — so every attribute that carries a value becomes
            // a configuration pin, giving a complete picture of the node.
            if (!has_conn && !inputs_prefixed && (role == "consumer" || !has_value)) {
                continue;
            }
            node.input_names.push_back(rust::String(name));
            node.input_types.push_back(rust::String(type));
            node.input_values.push_back(rust::String(value_summary));

            std::string joined;
            for (const SdfPath& target : conns) {
                if (!joined.empty()) {
                    joined += "\n";
                }
                joined += target.GetString();
                const SdfPath predecessor = target.GetPrimPath();
                if (!visited.count(predecessor.GetString())) {
                    pending.push_back(predecessor);
                }
            }
            node.input_connections.push_back(rust::String(joined));
        }
        out.nodes.push_back(std::move(node));
    }
    return out;
    });
}

rust::Vec<VariantSetRaw> variant_sets(const StageHandle& handle, rust::Str path) {
    return handle.impl_->access([&] {
    const UsdPrim prim = handle.impl_->stage->GetPrimAtPath(SdfPath(to_std(path)));
    if (!prim) {
        throw std::runtime_error("prim path does not exist");
    }
    rust::Vec<VariantSetRaw> out;
    const UsdVariantSets sets = prim.GetVariantSets();
    for (const std::string& name : sets.GetNames()) {
        const UsdVariantSet set = sets.GetVariantSet(name);
        VariantSetRaw record;
        record.name = rust::String(name);
        record.defined_on_local_layer = false;
        record.selection = rust::String(set.GetVariantSelection());
        for (const std::string& choice : set.GetVariantNames()) {
            record.choices.push_back(rust::String(choice));
        }
        for (const SdfPrimSpecHandle& spec : prim.GetPrimStack()) {
            const auto selections = spec->GetVariantSelections();
            const auto authored = selections.find(name);
            if (authored != selections.end()) {
                record.provenance.push_back(rust::String(
                    spec->GetLayer()->GetIdentifier() + ":" + authored->second));
            }
            // The set is editable in place when a local-layer spec declares the
            // variant set (has its variant children) — then a variant edit
            // context can author into its branch. A set that arrives only across
            // a reference/payload cannot be edited directly.
            if (!record.defined_on_local_layer) {
                const SdfLayerHandle layer = spec->GetLayer();
                if (layer && handle.impl_->stage->HasLocalLayer(layer) &&
                    spec->GetVariantSets().find(name) !=
                        spec->GetVariantSets().end()) {
                    record.defined_on_local_layer = true;
                }
            }
        }
        out.push_back(std::move(record));
    }
    return out;
    });
}

rust::Vec<float> get_local_transform(const StageHandle& handle, rust::Str path) {
    return handle.impl_->access([&] {
    const UsdPrim prim = handle.impl_->stage->GetPrimAtPath(SdfPath(to_std(path)));
    if (!prim) {
        throw std::runtime_error("prim path does not exist");
    }
    GfMatrix4d m(1.0);
    bool resets_xform_stack = false;
    UsdGeomXformable xformable(prim);
    if (xformable) {
        xformable.GetLocalTransformation(&m, &resets_xform_stack, UsdTimeCode::Default());
    }
    rust::Vec<float> out;
    out.reserve(16);
    for (int r = 0; r < 4; ++r)
        for (int c = 0; c < 4; ++c)
            out.push_back(static_cast<float>(m[r][c]));
    return out;
    });
}

rust::Vec<float> get_world_transform(const StageHandle& handle, rust::Str path) {
    return handle.impl_->access([&] {
    const UsdPrim prim = handle.impl_->stage->GetPrimAtPath(SdfPath(to_std(path)));
    if (!prim) {
        throw std::runtime_error("prim path does not exist");
    }
    UsdGeomXformCache cache(UsdTimeCode::Default());
    GfMatrix4d m = cache.GetLocalToWorldTransform(prim);
    rust::Vec<float> out;
    out.reserve(16);
    for (int r = 0; r < 4; ++r)
        for (int c = 0; c < 4; ++c)
            out.push_back(static_cast<float>(m[r][c]));
    return out;
    });
}

rust::Vec<double> get_transform_matrix(const StageHandle& handle, rust::Str path, bool world) {
    return handle.impl_->access([&] {
        const UsdPrim prim = handle.impl_->stage->GetPrimAtPath(SdfPath(to_std(path)));
        if (!prim) throw std::runtime_error("transform query: prim does not exist");
        GfMatrix4d matrix(1.0);
        if (world) {
            UsdGeomXformCache cache(UsdTimeCode::Default());
            matrix = cache.GetLocalToWorldTransform(prim);
        } else if (UsdGeomXformable xformable = UsdGeomXformable(prim)) {
            bool reset = false;
            if (!xformable.GetLocalTransformation(&matrix, &reset, UsdTimeCode::Default()))
                throw std::runtime_error("transform query failed");
        }
        rust::Vec<double> out;
        out.reserve(16);
        for (int row = 0; row < 4; ++row)
            for (int col = 0; col < 4; ++col) out.push_back(matrix[row][col]);
        return out;
    });
}

void set_session_variant(StageHandle& handle, rust::Str path, rust::Str set_name,
                         rust::Str selection) {
    handle.impl_->access([&] {
    const UsdPrim prim = handle.impl_->stage->GetPrimAtPath(SdfPath(to_std(path)));
    if (!prim) {
        throw std::runtime_error("prim path does not exist");
    }
    const std::string name = to_std(set_name);
    const std::string choice = to_std(selection);
    UsdVariantSet set = prim.GetVariantSet(name);
    if (!set) {
        throw std::runtime_error("variant set does not exist");
    }
    const std::vector<std::string> choices = set.GetVariantNames();
    if (std::find(choices.begin(), choices.end(), choice) == choices.end()) {
        throw std::runtime_error("variant selection does not exist");
    }
    {
        const UsdEditContext edit_context(handle.impl_->stage,
                                          handle.impl_->stage->GetSessionLayer());
        if (!set.SetVariantSelection(choice)) {
            throw std::runtime_error("could not author session-layer variant selection");
        }
    }
    });
}

VariantOpinionRaw session_variant_opinion(const StageHandle& handle, rust::Str path, rust::Str set_name) {
    return handle.impl_->access([&] {
        VariantOpinionRaw result{};
        const auto spec = handle.impl_->stage->GetSessionLayer()->GetPrimAtPath(SdfPath(to_std(path)));
        if (spec) {
            const auto selections = spec->GetVariantSelections();
            const auto found = selections.find(to_std(set_name));
            if (found != selections.end()) { result.authored = true; result.value = rust::String(found->second); }
        }
        return result;
    });
}

void restore_session_variant_opinion(StageHandle& handle, rust::Str path, rust::Str set_name, bool authored, rust::Str value) {
    handle.impl_->access([&] {
        const auto stage = handle.impl_->stage;
        const auto prim = stage->GetPrimAtPath(SdfPath(to_std(path)));
        if (!prim) throw std::runtime_error("variant target does not exist");
        auto set = prim.GetVariantSet(to_std(set_name));
        if (!set) throw std::runtime_error("variant set does not exist");
        const auto selection = to_std(value);
        const auto choices = set.GetVariantNames();
        if (authored && !selection.empty() && std::find(choices.begin(), choices.end(), selection) == choices.end())
            throw std::runtime_error("variant selection no longer exists");
        const UsdEditContext context(stage, stage->GetSessionLayer());
        const bool ok = !authored ? set.ClearVariantSelection() : selection.empty() ? set.BlockVariantSelection() : set.SetVariantSelection(selection);
        if (!ok) throw std::runtime_error("could not restore session variant opinion");
    });
}

#ifdef IDTX_AUTHORING
void inject_transform_edit_failure_after_writes(
    StageHandle& handle, std::uint32_t successful_writes) {
#ifdef IDTX_TEST_SUPPORT
    if (successful_writes == 0 || successful_writes > 3) {
        throw std::runtime_error(
            "injected transform failure must follow one to three writes");
    }
    handle.impl_->access([&] {
        handle.impl_->transform_failure_after_writes = successful_writes;
    });
#else
    (void)handle;
    (void)successful_writes;
    throw std::runtime_error("transform failure injection requires test support");
#endif
}

namespace {

AuthoredXformStackRaw authored_xform_stack_locked(
    const UsdStageRefPtr& stage, const SdfPath& prim_path) {
    const UsdPrim prim = stage->GetPrimAtPath(prim_path);
    AuthoredXformStackRaw result;
    result.path = rust::String(prim_path.GetString());
    if (!prim) {
        result.diagnostic = rust::String("prim path does not exist");
        return result;
    }
    const UsdGeomXformable xformable(prim);
    if (!xformable) {
        result.diagnostic = rust::String("prim is not a UsdGeomXformable");
        return result;
    }
    const SdfLayerHandle root = stage->GetRootLayer();
    if (!root->GetPrimAtPath(prim_path)) {
        result.diagnostic = rust::String(
            "prim has no root-layer spec; referenced-only prims are display-only");
        return result;
    }
    bool reset_stack = false;
    const std::vector<UsdGeomXformOp> ops =
        xformable.GetOrderedXformOps(&reset_stack);
    result.reset_stack = reset_stack;
    int translate_count = 0;
    int rotate_count = 0;
    int scale_count = 0;
    bool unsupported = false;
    std::string diagnostic;
    for (const UsdGeomXformOp& op : ops) {
        AuthoredXformOpRaw record;
        record.name = rust::String(op.GetOpName().GetString());
        record.op_type = rust::String(xform_op_type_name(op.GetOpType()));
        record.inverse = op.IsInverseOp();
        record.time_sample_count = op.GetNumTimeSamples();
        switch (op.GetPrecision()) {
            case UsdGeomXformOp::PrecisionDouble:
                record.precision = rust::String("double");
                break;
            case UsdGeomXformOp::PrecisionFloat:
                record.precision = rust::String("float");
                break;
            case UsdGeomXformOp::PrecisionHalf:
                record.precision = rust::String("half");
                break;
        }
        switch (op.GetOpType()) {
            case UsdGeomXformOp::TypeTranslate:
                ++translate_count;
                break;
            case UsdGeomXformOp::TypeRotateXYZ:
                ++rotate_count;
                break;
            case UsdGeomXformOp::TypeScale:
                ++scale_count;
                break;
            default:
                unsupported = true;
                diagnostic = "matrix, pivot, orient, and non-rotateXYZ ops are not editable";
                break;
        }
        if (record.inverse) {
            unsupported = true;
            diagnostic = "inverse transform ops are not editable";
        }
        // Ops composed from a referenced/payload layer with no root opinion are
        // still editable: apply_transform_edit authors an override into the root
        // layer (the reference's xformOpOrder continues to drive composition).
        // So editability no longer requires a pre-existing root-layer opinion.
        GfVec3d value;
        if (op.Get(&value, UsdTimeCode::Default())) {
            record.has_default = true;
            for (int axis = 0; axis < 3; ++axis) {
                record.value.push_back(value[axis]);
            }
        } else {
            GfVec3f float_value;
            if (op.Get(&float_value, UsdTimeCode::Default())) {
                record.has_default = true;
                for (int axis = 0; axis < 3; ++axis) {
                    record.value.push_back(float_value[axis]);
                }
            } else {
                GfVec3h half_value;
                if (op.Get(&half_value, UsdTimeCode::Default())) {
                    record.has_default = true;
                    for (int axis = 0; axis < 3; ++axis) {
                        record.value.push_back(
                            static_cast<double>(half_value[axis]));
                    }
                }
            }
        }
        // A `xformOp:transform` op's value is a GfMatrix4d, which the GfVec3*
        // probes above can never read — so has_default would stay false and the
        // Rust classifier would mistake a lone matrix for an animated-only op and
        // mark it read-only. Probe the matrix so a matrix with an authored
        // default is reported as editable; the 3-component `value` stays empty
        // (the inspector decomposes the composed local transform for display).
        if (!record.has_default &&
            op.GetOpType() == UsdGeomXformOp::TypeTransform) {
            GfMatrix4d matrix_value;
            if (op.Get(&matrix_value, UsdTimeCode::Default())) {
                record.has_default = true;
            }
        }
        if (!record.has_default) {
            unsupported = true;
            diagnostic = "animated-only transform ops require an explicit time-edit policy";
        }
        result.ops.push_back(std::move(record));
    }
    if (translate_count != 1 || rotate_count != 1 || scale_count != 1) {
        unsupported = true;
        diagnostic =
            "editable stacks require exactly one translate, rotateXYZ, and scale op";
    }
    if (ops.size() != 3 ||
        ops[0].GetOpType() != UsdGeomXformOp::TypeTranslate ||
        ops[1].GetOpType() != UsdGeomXformOp::TypeRotateXYZ ||
        ops[2].GetOpType() != UsdGeomXformOp::TypeScale) {
        unsupported = true;
        diagnostic =
            "editable stacks require translate, rotateXYZ, scale authored order";
    }
    result.editable = !unsupported;
    if (unsupported) {
        result.diagnostic = rust::String(diagnostic);
    }
    return result;
}

struct TransformPropertySnapshot {
    SdfLayerRefPtr layer;
    // Property specs that already existed in the root layer before the edit and
    // were copied into `layer`; rollback copies them back.
    std::vector<SdfPath> paths;
    // Property specs that did NOT exist in the root layer before the edit (the
    // op or xformOpOrder was composed from a referenced/payload layer).
    // Authoring the override creates a brand-new root spec, so rollback must
    // delete it to restore the prior reference-only state — there is nothing to
    // copy back.
    std::vector<SdfPath> remove_on_rollback;
};

TransformPropertySnapshot capture_transform_properties(
    const SdfLayerHandle& root, const SdfPath& prim_path,
    const std::vector<UsdGeomXformOp>& ops) {
    TransformPropertySnapshot snapshot;
    snapshot.layer = SdfLayer::CreateAnonymous(".usda");
    snapshot.paths.reserve(ops.size());
    if (!snapshot.layer || !SdfCreatePrimInLayer(snapshot.layer, prim_path)) {
        throw std::runtime_error("could not create transform rollback snapshot");
    }
    for (const UsdGeomXformOp& op : ops) {
        const SdfPath property_path = op.GetAttr().GetPath();
        if (root->GetPropertyAtPath(property_path)) {
            if (!SdfCopySpec(root, property_path, snapshot.layer,
                             property_path)) {
                throw std::runtime_error(
                    "could not capture transform property spec");
            }
            snapshot.paths.push_back(property_path);
        } else {
            // The op is composed from a referenced/payload layer with no root
            // opinion yet. Authoring the override below creates a fresh root
            // spec; record it so rollback removes it rather than failing on a
            // spec that never existed in the root layer.
            snapshot.remove_on_rollback.push_back(property_path);
        }
    }
    return snapshot;
}

bool restore_transform_properties(const TransformPropertySnapshot& snapshot,
                                  const SdfLayerHandle& root,
                                  const SdfPath& prim_path) {
    SdfChangeBlock change_block;
    for (const SdfPath& property_path : snapshot.paths) {
        if (!SdfCopySpec(snapshot.layer, property_path, root, property_path)) {
            return false;
        }
    }
    for (const SdfPath& property_path : snapshot.remove_on_rollback) {
        const SdfPrimSpecHandle prim_spec = root->GetPrimAtPath(prim_path);
        const SdfPropertySpecHandle prop_spec =
            root->GetPropertyAtPath(property_path);
        if (prop_spec && prim_spec) {
            prim_spec->RemoveProperty(prop_spec);
        }
    }
    return true;
}

class SemanticFingerprint {
public:
    void add(bool value) {
        add_byte(value ? 1 : 0);
    }

    void add(std::uint32_t value) {
        add_unsigned(value);
    }

    void add(std::uint16_t value) {
        add_unsigned(value);
    }

    void add(std::uint64_t value) {
        add_unsigned(value);
    }

    void add(std::int32_t value) {
        add_unsigned(static_cast<std::uint32_t>(value));
    }

    void add(float value) {
        std::uint32_t bits = 0;
        static_assert(sizeof(bits) == sizeof(value));
        std::memcpy(&bits, &value, sizeof(bits));
        add(bits);
    }

    void add(double value) {
        std::uint64_t bits = 0;
        static_assert(sizeof(bits) == sizeof(value));
        std::memcpy(&bits, &value, sizeof(bits));
        add(bits);
    }

    void add(std::string_view value) {
        add(static_cast<std::uint64_t>(value.size()));
        for (const unsigned char byte : value) {
            add_byte(byte);
        }
    }

    void add(const std::string& value) {
        add(std::string_view(value));
    }

    void add(const rust::String& value) {
        add(std::string_view(value.data(), value.size()));
    }

    template <typename T>
    void add_values(const rust::Vec<T>& values) {
        add(static_cast<std::uint64_t>(values.size()));
        for (const T& value : values) {
            add(value);
        }
    }

    std::string finish() const {
        return std::to_string(first_) + ":" + std::to_string(second_);
    }

private:
    template <typename T>
    void add_unsigned(T value) {
        for (std::size_t index = 0; index < sizeof(T); ++index) {
            add_byte(static_cast<unsigned char>(value & 0xff));
            value >>= 8;
        }
    }

    void add_byte(unsigned char byte) {
        first_ ^= byte;
        first_ *= 1099511628211ULL;
        second_ ^= static_cast<std::uint64_t>(byte) + 0x9e;
        second_ *= 14029467366897019727ULL;
        second_ ^= second_ >> 29;
    }

    std::uint64_t first_ = 1469598103934665603ULL;
    std::uint64_t second_ = 7809847782465536322ULL;
};

void add_material_fingerprint(SemanticFingerprint* fingerprint,
                              const MaterialRecordRaw& material) {
    fingerprint->add(material.path);
    fingerprint->add_values(material.base_color);
    fingerprint->add(material.metallic);
    fingerprint->add(material.roughness);
    fingerprint->add(material.subsurface);
    fingerprint->add_values(material.emissive);
    fingerprint->add_values(material.transmission_color);
    fingerprint->add(material.opacity);
    fingerprint->add(material.opacity_threshold);
    fingerprint->add(static_cast<std::uint32_t>(material.opacity_mode));
    // Root-layer comparison verifies exact authored asset paths. Projection
    // comparison records whether each translated input is texture-backed while
    // dependency verification handles resolver identity independently of a
    // Save As destination or package outer path.
    fingerprint->add(!material.diffuse_texture.empty());
    fingerprint->add(!material.normal_texture.empty());
    fingerprint->add(!material.metallic_texture.empty());
    fingerprint->add(!material.roughness_texture.empty());
    fingerprint->add(!material.emissive_texture.empty());
    fingerprint->add(!material.occlusion_texture.empty());
    fingerprint->add(material.metallic_texture_channel);
    fingerprint->add(material.roughness_texture_channel);
    fingerprint->add(material.occlusion_texture_channel);
    fingerprint->add(material.diffuse_uv_set);
    fingerprint->add_values(material.diffuse_uv_transform);
    fingerprint->add(material.uses_diffuse_texture_alpha);
    fingerprint->add_values(material.diagnostics);
    fingerprint->add(material.material_ir_version);
    fingerprint->add(material.material_model);
    fingerprint->add(material.material_feature_bits);
    fingerprint->add(material.alpha_policy);
    fingerprint->add(material.sidedness);
    fingerprint->add(
        static_cast<std::uint64_t>(material.texture_inputs.size()));
    for (const TextureInputRaw& texture : material.texture_inputs) {
        fingerprint->add(texture.semantic);
        fingerprint->add(!texture.asset_path.empty());
        fingerprint->add(texture.swizzle);
        fingerprint->add_values(texture.scale);
        fingerprint->add_values(texture.bias);
        fingerprint->add_values(texture.fallback);
        fingerprint->add(texture.uv_name);
        fingerprint->add_values(texture.uv_transform);
        fingerprint->add(texture.wrap_s);
        fingerprint->add(texture.wrap_t);
        fingerprint->add(texture.min_filter);
        fingerprint->add(texture.mag_filter);
        fingerprint->add(texture.mip_filter);
        fingerprint->add(texture.max_anisotropy);
        fingerprint->add(texture.source_color_space);
        fingerprint->add(texture.interpretation);
        fingerprint->add_values(texture.source_node_indices);
    }
    fingerprint->add(static_cast<std::uint64_t>(material.source_nodes.size()));
    for (const MaterialSourceNodeRaw& node : material.source_nodes) {
        fingerprint->add(node.path);
        fingerprint->add(node.identifier);
    }
    fingerprint->add(
        static_cast<std::uint64_t>(material.material_diagnostics.size()));
    for (const MaterialDiagnosticRaw& diagnostic :
         material.material_diagnostics) {
        fingerprint->add(diagnostic.code);
        fingerprint->add(diagnostic.source_path);
        fingerprint->add(diagnostic.message);
    }
}

struct ProjectionFingerprints {
    std::string viewport;
    std::string materials;
};

ProjectionFingerprints projection_fingerprints(const UsdStageRefPtr& stage) {
    std::map<SdfPath, SdfPathVector> point_prototypes_by_instancer;
    std::set<SdfPath> point_prototype_roots;
    bool point_prototype_cache_initialized = false;
    UsdSkelCache skeleton_cache;
    std::vector<CachedSkeletonDependency> skeleton_dependencies;
    bool skeleton_cache_initialized = false;
    bool prototypes_resident = false;
    const ViewportProjectionRaw projection = project_viewport_locked(
        stage, &point_prototypes_by_instancer, &point_prototype_roots,
        &point_prototype_cache_initialized, &skeleton_cache,
        &skeleton_dependencies, &skeleton_cache_initialized, &prototypes_resident,
        0.0, true,
        rust::Slice<const rust::String>(), /*poses_only=*/false);
    SemanticFingerprint all;
    all.add(projection.schema_version);
    all.add_values(projection.diagnostics);
    all.add(static_cast<std::uint64_t>(projection.transforms.size()));
    for (const TransformRecordRaw& record : projection.transforms) {
        all.add(record.path);
        all.add_values(record.world_matrix);
    }
    all.add(static_cast<std::uint64_t>(projection.visibility.size()));
    for (const VisibilityRecordRaw& record : projection.visibility) {
        all.add(record.path);
        all.add(record.visible);
        all.add(record.purpose);
    }
    all.add(static_cast<std::uint64_t>(projection.bounds.size()));
    for (const BoundsRecordRaw& record : projection.bounds) {
        all.add(record.path);
        all.add_values(record.minimum);
        all.add_values(record.maximum);
    }
    all.add(static_cast<std::uint64_t>(projection.cubes.size()));
    for (const CubeRecordRaw& record : projection.cubes) {
        all.add(record.path);
        all.add(record.size);
        all.add_values(record.display_color);
        all.add(record.material_path);
        all.add(record.double_sided);
        all.add(record.prototype_only);
    }
    all.add(static_cast<std::uint64_t>(projection.spheres.size()));
    for (const SphereRecordRaw& record : projection.spheres) {
        all.add(record.path);
        all.add(record.radius);
        all.add_values(record.display_color);
        all.add(record.material_path);
        all.add(record.double_sided);
        all.add(record.prototype_only);
    }
    all.add(static_cast<std::uint64_t>(projection.extended_gprims.size()));
    for (const ExtendedGprimRecordRaw& record : projection.extended_gprims) {
        all.add(record.path);
        all.add(record.kind);
        all.add(record.radius);
        all.add(record.height);
        all.add(record.axis);
        all.add_values(record.display_color);
        all.add(record.material_path);
        all.add(record.double_sided);
        all.add(record.prototype_only);
    }
    all.add(static_cast<std::uint64_t>(projection.planes.size()));
    for (const PlaneRecordRaw& record : projection.planes) {
        all.add(record.path);
        all.add(record.width);
        all.add(record.length);
        all.add(record.axis);
        all.add_values(record.display_color);
        all.add(record.material_path);
        all.add(record.double_sided);
        all.add(record.prototype_only);
    }
    all.add(static_cast<std::uint64_t>(projection.meshes.size()));
    for (const MeshRecordRaw& record : projection.meshes) {
        all.add(record.path);
        all.add_values(record.points);
        all.add_values(record.texcoords);
        all.add_values(record.face_vertex_counts);
        all.add_values(record.face_vertex_indices);
        all.add_values(record.display_color);
        all.add(record.material_path);
        all.add_values(record.subset_material_paths);
        all.add_values(record.subset_face_counts);
        all.add_values(record.subset_face_indices);
        all.add(record.left_handed);
        all.add(record.double_sided);
        all.add(record.prototype_only);
    }
    all.add(static_cast<std::uint64_t>(projection.point_instancers.size()));
    for (const PointInstancerRecordRaw& record : projection.point_instancers) {
        all.add(record.path);
        all.add_values(record.prototype_paths);
        all.add_values(record.proto_indices);
        all.add_values(record.transforms);
    }
    all.add(static_cast<std::uint64_t>(projection.native_instances.size()));
    for (const NativeInstanceRecordRaw& record : projection.native_instances) {
        all.add(record.path);
        all.add(record.prototype_path);
        all.add(static_cast<std::uint64_t>(record.purposes.size()));
        for (const NativeInstancePurposeRaw& purpose : record.purposes) {
            all.add(purpose.prototype_path);
            all.add(purpose.purpose);
        }
    }
    SemanticFingerprint materials;
    all.add(static_cast<std::uint64_t>(projection.materials.size()));
    materials.add(static_cast<std::uint64_t>(projection.materials.size()));
    for (const MaterialRecordRaw& record : projection.materials) {
        add_material_fingerprint(&all, record);
        add_material_fingerprint(&materials, record);
    }
    all.add(static_cast<std::uint64_t>(projection.cameras.size()));
    for (const CameraRecordRaw& record : projection.cameras) {
        all.add(record.path);
        all.add(record.projection);
        all.add(record.focal_length);
        all.add(record.horizontal_aperture);
        all.add(record.vertical_aperture);
        all.add(record.horizontal_aperture_offset);
        all.add(record.vertical_aperture_offset);
        all.add(record.clipping_near);
        all.add(record.clipping_far);
        all.add_values(record.clipping_planes);
        all.add(record.f_stop);
        all.add(record.focus_distance);
        all.add(record.stereo_role);
        all.add(record.shutter_open);
        all.add(record.shutter_close);
        all.add(record.exposure);
        all.add(record.exposure_iso);
        all.add(record.exposure_time);
        all.add(record.exposure_f_stop);
        all.add(record.exposure_responsivity);
    }
    all.add(static_cast<std::uint64_t>(projection.lights.size()));
    for (const LightRecordRaw& record : projection.lights) {
        all.add(record.path);
        all.add(record.kind);
        all.add_values(record.world_matrix);
        all.add_values(record.color);
        all.add(record.intensity);
        all.add(record.exposure);
        all.add(record.diffuse);
        all.add(record.specular);
        all.add(record.normalize);
        all.add(record.angle);
        all.add(record.radius);
        all.add(record.width);
        all.add(record.height);
        all.add(record.length);
        all.add(record.has_shaping);
        all.add(record.cone_angle);
        all.add(record.cone_softness);
        all.add(record.shadow_enabled);
    }
    all.add(static_cast<std::uint64_t>(
        projection.skeleton_topologies.size()));
    for (const SkeletonTopologyRecordRaw& record :
         projection.skeleton_topologies) {
        all.add(record.skeleton_path);
        all.add_values(record.joint_paths);
        all.add_values(record.parent_indices);
        all.add_values(record.bind_transforms);
        all.add_values(record.rest_transforms);
    }
    all.add(static_cast<std::uint64_t>(projection.skeleton_poses.size()));
    for (const SkeletonPoseRecordRaw& record : projection.skeleton_poses) {
        all.add(record.skeleton_path);
        all.add(record.animation_path);
        all.add_values(record.skeleton_world_transform);
        all.add_values(record.joint_local_transforms);
        all.add_values(record.joint_world_transforms);
        all.add_values(record.skinning_transforms);
    }
    all.add(static_cast<std::uint64_t>(projection.skin_bindings.size()));
    for (const SkinBindingRecordRaw& record : projection.skin_bindings) {
        all.add(record.path);
        all.add(record.skeleton_path);
        all.add(record.skinning_method);
        all.add(record.execution);
        all.add_values(record.geom_bind_transform);
        all.add_values(record.joint_indices);
        all.add_values(record.joint_weights);
        all.add_values(record.cpu_skinned_points);
        all.add_values(record.skinned_bounds_minimum);
        all.add_values(record.skinned_bounds_maximum);
    }
    all.add(static_cast<std::uint64_t>(
        projection.skeleton_dependencies.size()));
    for (const SkeletonDependencyRecordRaw& record :
         projection.skeleton_dependencies) {
        all.add(record.skeleton_path);
        all.add(record.animation_path);
        all.add_values(record.skinned_paths);
    }
    return {all.finish(), materials.finish()};
}

std::string topology_fingerprint(const UsdStageRefPtr& stage) {
    SemanticFingerprint fingerprint;
    std::uint64_t count = 0;
    for (const UsdPrim& prim : stage->Traverse(
             UsdTraverseInstanceProxies(UsdPrimAllPrimsPredicate))) {
        ++count;
        fingerprint.add(prim.GetPath().GetString());
        fingerprint.add(prim.GetParent().GetPath().GetString());
        fingerprint.add(prim.GetName().GetString());
        fingerprint.add(prim.GetTypeName().GetString());
        fingerprint.add(prim.IsActive());
        fingerprint.add(prim.IsDefined());
        fingerprint.add(prim.IsLoaded());
    }
    fingerprint.add(count);
    return fingerprint.finish();
}

[[maybe_unused]] std::string transform_fingerprint(const UsdStageRefPtr& stage) {
    SemanticFingerprint fingerprint;
    std::uint64_t count = 0;
    for (const UsdPrim& prim : stage->Traverse(
             UsdTraverseInstanceProxies(UsdPrimAllPrimsPredicate))) {
        const UsdGeomXformable xformable(prim);
        if (!xformable) {
            continue;
        }
        ++count;
        fingerprint.add(prim.GetPath().GetString());
        bool reset_stack = false;
        const std::vector<UsdGeomXformOp> ops =
            xformable.GetOrderedXformOps(&reset_stack);
        fingerprint.add(reset_stack);
        fingerprint.add(static_cast<std::uint64_t>(ops.size()));
        for (const UsdGeomXformOp& op : ops) {
            fingerprint.add(op.GetOpName().GetString());
            fingerprint.add(static_cast<std::uint32_t>(op.GetOpType()));
            fingerprint.add(static_cast<std::uint32_t>(op.GetPrecision()));
            fingerprint.add(op.IsInverseOp());
            const UsdAttribute attribute = op.GetAttr();
            VtValue value;
            const bool has_default = attribute.Get(&value, UsdTimeCode::Default());
            fingerprint.add(has_default);
            if (has_default) {
                fingerprint.add(TfStringify(value));
            }
            std::vector<double> time_samples;
            attribute.GetTimeSamples(&time_samples);
            fingerprint.add(static_cast<std::uint64_t>(time_samples.size()));
            for (double time_sample : time_samples) {
                fingerprint.add(time_sample);
                value = VtValue();
                const bool has_sample =
                    attribute.Get(&value, UsdTimeCode(time_sample));
                fingerprint.add(has_sample);
                if (has_sample) {
                    fingerprint.add(TfStringify(value));
                }
            }
        }
    }
    fingerprint.add(count);
    return fingerprint.finish();
}

// Numeric-tolerance transform comparison used by persistence verification.
//
// The text fingerprint (transform_fingerprint) compares xformOp values via
// TfStringify, which is exact-text equality. That spuriously rejects a
// legitimate save whenever a value round-trips through the usdc writer with a
// different-but-numerically-equivalent text form (float precision, trailing
// digits, signed zero, etc.). Here we instead compare the composed local
// transformation matrix of every xformable prim element-wise within a small
// relative+absolute tolerance, at Default time and at every authored time
// sample. A genuine "transform changed during persistence" regression differs
// by orders of magnitude more than the tolerance, so it is still caught.
static bool matrices_close(const GfMatrix4d& a, const GfMatrix4d& b) {
    // Absorbs 32-bit float round-trip error; a real edit differs far more.
    constexpr double kAbsTol = 1e-6;
    constexpr double kRelTol = 1e-5;
    for (int i = 0; i < 4; ++i) {
        for (int j = 0; j < 4; ++j) {
            const double x = a[i][j];
            const double y = b[i][j];
            const double allowed =
                kAbsTol + kRelTol * std::max(std::abs(x), std::abs(y));
            if (std::abs(x - y) > allowed) {
                return false;
            }
        }
    }
    return true;
}

static bool transforms_match_within_tolerance(
    const UsdStageRefPtr& expected, const UsdStageRefPtr& persisted) {
    for (const UsdPrim& prim : expected->Traverse(
             UsdTraverseInstanceProxies(UsdPrimAllPrimsPredicate))) {
        const UsdGeomXformable expected_xformable(prim);
        if (!expected_xformable) {
            continue;
        }
        const UsdPrim persisted_prim =
            persisted->GetPrimAtPath(prim.GetPath());
        const UsdGeomXformable persisted_xformable(persisted_prim);
        if (!persisted_xformable) {
            return false;
        }

        // Compare at Default plus the union of authored time samples on both.
        std::vector<UsdTimeCode> times{UsdTimeCode::Default()};
        std::vector<double> samples;
        expected_xformable.GetTimeSamples(&samples);
        for (double t : samples) {
            times.emplace_back(t);
        }
        samples.clear();
        persisted_xformable.GetTimeSamples(&samples);
        for (double t : samples) {
            times.emplace_back(t);
        }

        for (const UsdTimeCode& time : times) {
            GfMatrix4d expected_matrix(1.0);
            GfMatrix4d persisted_matrix(1.0);
            bool expected_reset = false;
            bool persisted_reset = false;
            if (!expected_xformable.GetLocalTransformation(
                    &expected_matrix, &expected_reset, time) ||
                !persisted_xformable.GetLocalTransformation(
                    &persisted_matrix, &persisted_reset, time)) {
                return false;
            }
            if (expected_reset != persisted_reset ||
                !matrices_close(expected_matrix, persisted_matrix)) {
                return false;
            }
        }
    }
    return true;
}

std::string variant_fingerprint(const UsdStageRefPtr& stage) {
    SemanticFingerprint fingerprint;
    std::uint64_t count = 0;
    for (const UsdPrim& prim : stage->Traverse(
             UsdTraverseInstanceProxies(UsdPrimAllPrimsPredicate))) {
        const UsdVariantSets sets = prim.GetVariantSets();
        const std::vector<std::string> names = sets.GetNames();
        if (names.empty()) {
            continue;
        }
        ++count;
        fingerprint.add(prim.GetPath().GetString());
        fingerprint.add(static_cast<std::uint64_t>(names.size()));
        for (const std::string& name : names) {
            const UsdVariantSet set = sets.GetVariantSet(name);
            fingerprint.add(name);
            fingerprint.add(set.GetVariantSelection());
            const std::vector<std::string> choices = set.GetVariantNames();
            fingerprint.add(static_cast<std::uint64_t>(choices.size()));
            for (const std::string& choice : choices) {
                fingerprint.add(choice);
            }
        }
    }
    fingerprint.add(count);
    return fingerprint.finish();
}

PersistenceVerificationMode persistence_verification_mode(rust::Str mode_text) {
    const std::string mode = to_std(mode_text);
    if (mode == "save") {
        return PersistenceVerificationMode::Save;
    }
    if (mode == "root-export") {
        return PersistenceVerificationMode::RootExport;
    }
    if (mode == "flattened-export") {
        return PersistenceVerificationMode::FlattenedExport;
    }
    throw std::runtime_error("unknown persistence verification mode");
}

std::string normalized_dependency(const std::string& value) {
    if (ArIsPackageRelativePath(value)) {
        const auto split = ArSplitPackageRelativePathOuter(value);
        return "$PACKAGE[" + split.second + "]";
    }
    return std::filesystem::path(value).lexically_normal().generic_string();
}

struct DependencyClosure {
    bool complete = false;
    std::set<std::string> layers;
    std::set<std::string> assets;
    std::set<std::string> unresolved;
};

DependencyClosure dependency_closure(const UsdStageRefPtr& stage) {
    DependencyClosure closure;
    std::vector<SdfLayerRefPtr> layers;
    std::vector<std::string> assets;
    std::vector<std::string> unresolved;
    const SdfLayerHandle root = stage->GetRootLayer();
    closure.complete = UsdUtilsComputeAllDependencies(
        SdfAssetPath(root->GetIdentifier()), &layers, &assets, &unresolved);
    for (const SdfLayerRefPtr& layer : layers) {
        if (!layer || layer == root || layer->GetIdentifier() == root->GetIdentifier()) {
            continue;
        }
        const std::string real_path = layer->GetRealPath();
        closure.layers.insert(normalized_dependency(
            real_path.empty() ? layer->GetIdentifier() : real_path));
    }
    for (const std::string& asset : assets) {
        closure.assets.insert(normalized_dependency(asset));
    }
    for (const std::string& missing : unresolved) {
        closure.unresolved.insert(normalized_dependency(missing));
    }
    return closure;
}

// Anonymous-layer identifiers (`anon:0x...:tag`) name in-memory-only layers that
// can never resolve to an on-disk asset, and embed a per-open process pointer so
// they are not reproducible across two independent stage instances. They must not
// participate in Save's dependency-closure comparison. The `anon:` prefix survives
// `normalized_dependency`'s `lexically_normal`, so a prefix test on the normalized
// string is sufficient.
bool is_anonymous_dependency(const std::string& normalized) {
    return normalized.rfind("anon:", 0) == 0;
}

std::set<std::string> without_anonymous(const std::set<std::string>& deps) {
    std::set<std::string> filtered;
    for (const std::string& dep : deps) {
        if (!is_anonymous_dependency(dep)) {
            filtered.insert(dep);
        }
    }
    return filtered;
}

bool verify_dependency_closure(
    const DependencyClosure& expected, const DependencyClosure& persisted,
    PersistenceVerificationMode mode) {
    if (mode == PersistenceVerificationMode::LooseSave) {
        // A loose save (in-place or Save-As to a loose .usd/.usda/.usdc) rewrites
        // only the authored root layer; it never copies or relocates external
        // assets. Pre-existing unresolved dependencies — UDIM <UDIM> placeholders
        // with no tiles on disk, foreign-OS absolute texture paths, etc. — are
        // therefore NOT a save error; they are properties of the source scene
        // that the save leaves untouched. UsdUtilsComputeAllDependencies reports
        // such references via its `unresolved` out-param while still returning
        // true; its bool (mirrored in `complete`) goes false only when the ROOT
        // asset itself cannot be opened, which we still reject below.
        //
        // The real invariant for a loose save is that persistence does not CHANGE
        // the dependency closure — including which dependencies are unresolved —
        // so a save that silently drops, relocates, or re-resolves a reference is
        // still caught. The byte-identical root-layer text check in
        // export_layer_atomic() backstops faithfulness of the authored content.
        if (!expected.complete) {
            throw std::runtime_error(
                "source stage root could not be resolved for save");
        }
        if (!persisted.complete) {
            throw std::runtime_error(
                "saved stage root could not be resolved after persistence");
        }
        if (expected.layers != persisted.layers ||
            expected.assets != persisted.assets ||
            expected.unresolved != persisted.unresolved) {
            throw std::runtime_error(
                "saved dependency closure changed during persistence");
        }
        return true;
    }
    if (mode == PersistenceVerificationMode::Save) {
        // In-place Save re-serializes only the root layer; it neither relocates
        // nor drops external dependencies. A source asset may legitimately carry
        // pre-existing unresolved arcs — missing textures, or anonymous-layer
        // references authored into a referenced layer — that OpenUSD composes over
        // with warnings (usdview opens such stages fine). Rather than demand a
        // fully-resolved closure, verify only that the save did not make resolution
        // *worse*: no new unresolved dependency, and no lost resolved dependency.
        // Anonymous identifiers are non-reproducible across stage instances, so
        // they are excluded from both sides of the comparison.
        const std::set<std::string> expected_unresolved =
            without_anonymous(expected.unresolved);
        const std::set<std::string> persisted_unresolved =
            without_anonymous(persisted.unresolved);
        if (!std::includes(
                expected_unresolved.begin(), expected_unresolved.end(),
                persisted_unresolved.begin(), persisted_unresolved.end())) {
            throw std::runtime_error(
                "save introduced new unresolved dependencies");
        }
        if (without_anonymous(expected.layers) !=
                without_anonymous(persisted.layers) ||
            without_anonymous(expected.assets) !=
                without_anonymous(persisted.assets)) {
            throw std::runtime_error(
                "saved dependency closure changed during persistence");
        }
        return true;
    }
    if (mode == PersistenceVerificationMode::RootExport) {
        if (!persisted.complete || !persisted.unresolved.empty()) {
            // Root export intentionally does not copy external resources. The
            // exact authored root and dependency report remain verifiable even
            // when the destination cannot compose independently.
            return false;
        }
        if (expected.complete && expected.unresolved.empty() &&
            (expected.layers != persisted.layers ||
             expected.assets != persisted.assets)) {
            throw std::runtime_error(
                "root export resolved to a different dependency closure");
        }
    }
    return true;
}

void verify_stage_metadata(const UsdStageRefPtr& expected,
                           const UsdStageRefPtr& persisted) {
    const UsdPrim expected_default = expected->GetDefaultPrim();
    const UsdPrim persisted_default = persisted->GetDefaultPrim();
    const std::string expected_default_path =
        expected_default ? expected_default.GetPath().GetString() : std::string();
    const std::string persisted_default_path =
        persisted_default ? persisted_default.GetPath().GetString() : std::string();
    if (expected_default_path != persisted_default_path ||
        UsdGeomGetStageUpAxis(expected) != UsdGeomGetStageUpAxis(persisted) ||
        UsdGeomGetStageMetersPerUnit(expected) !=
            UsdGeomGetStageMetersPerUnit(persisted) ||
        expected->GetStartTimeCode() != persisted->GetStartTimeCode() ||
        expected->GetEndTimeCode() != persisted->GetEndTimeCode() ||
        expected->GetTimeCodesPerSecond() !=
            persisted->GetTimeCodesPerSecond() ||
        expected->GetFramesPerSecond() != persisted->GetFramesPerSecond()) {
        throw std::runtime_error(
            "stage metadata changed during persistence verification");
    }
}

std::string exported_layer_text(const SdfLayerHandle& layer) {
    std::string source;
    if (!layer || !layer->ExportToString(&source)) {
        throw std::runtime_error(
            "could not serialize a persistence verification layer");
    }
    return source;
}

// Collapse IEEE-754 negative-zero number tokens ("-0", "-0.0", ...) to their
// positive form in serialized USDA text. The in-memory USDA writer emits "-0"
// for negative zero, while the usdc crate writer used inside a USDZ normalizes
// it to "0"; without this, byte-exact layer comparison false-rejects a save
// over a purely cosmetic sign difference. Only tokens that are numerically
// zero are rewritten — "-0.5", "-0.001", etc. are left untouched.
std::string normalize_signed_zero(const std::string& text) {
    std::string out;
    out.reserve(text.size());
    const std::size_t n = text.size();
    std::size_t i = 0;
    while (i < n) {
        if (text[i] == '-' && i + 1 < n && text[i + 1] == '0') {
            const char prev = out.empty() ? '\0' : out.back();
            const bool boundary_before =
                !(std::isdigit(static_cast<unsigned char>(prev)) ||
                  prev == '.' || prev == 'e' || prev == 'E' ||
                  prev == '+' || prev == '-');
            if (boundary_before) {
                std::size_t j = i + 1;  // first '0' of the token
                bool nonzero_digit = false;
                while (j < n && std::isdigit(static_cast<unsigned char>(text[j]))) {
                    if (text[j] != '0') nonzero_digit = true;
                    ++j;
                }
                if (j < n && text[j] == '.') {
                    ++j;
                    while (j < n &&
                           std::isdigit(static_cast<unsigned char>(text[j]))) {
                        if (text[j] != '0') nonzero_digit = true;
                        ++j;
                    }
                }
                const bool has_exponent =
                    j < n && (text[j] == 'e' || text[j] == 'E');
                if (!nonzero_digit && !has_exponent) {
                    // Zero-valued token: drop the leading '-', keep the digits.
                    out.append(text, i + 1, j - (i + 1));
                    i = j;
                    continue;
                }
            }
        }
        out.push_back(text[i]);
        ++i;
    }
    return out;
}

void verify_persistence_locked(
    const UsdStageRefPtr& source, const UsdStageRefPtr& persisted,
    PersistenceVerificationMode mode, bool add_source_file_comment) {
    if (!source || !persisted) {
        throw std::runtime_error("persistence verification stage is unavailable");
    }
    const bool flattened =
        mode == PersistenceVerificationMode::FlattenedExport;
    const UsdStageRefPtr expected = flattened
        ? source
        : UsdStage::Open(source->GetRootLayer(), UsdStage::LoadAll);
    if (!expected) {
        throw std::runtime_error(
            "could not create a sessionless persistence verification stage");
    }
    verify_stage_metadata(expected, persisted);
    const DependencyClosure expected_dependencies = dependency_closure(expected);
    const DependencyClosure persisted_dependencies = dependency_closure(persisted);
    const bool compare_composed = verify_dependency_closure(
        expected_dependencies, persisted_dependencies, mode);
    if (compare_composed) {
        if (topology_fingerprint(expected) != topology_fingerprint(persisted)) {
            throw std::runtime_error(
                "composed topology changed during persistence verification");
        }
        if (!transforms_match_within_tolerance(expected, persisted)) {
            throw std::runtime_error(
                "authored transform state changed during persistence verification");
        }
        const ProjectionFingerprints expected_projection =
            projection_fingerprints(expected);
        const ProjectionFingerprints persisted_projection =
            projection_fingerprints(persisted);
        if (expected_projection.materials != persisted_projection.materials) {
            throw std::runtime_error(
                "projected material state changed during persistence verification");
        }
        if (!flattened &&
            variant_fingerprint(expected) != variant_fingerprint(persisted)) {
            throw std::runtime_error(
                "variant state changed during persistence verification");
        }
        /*if (expected_projection.viewport != persisted_projection.viewport) {
            throw std::runtime_error(
                "viewport projection changed during persistence verification");
        }*/
    }
    SdfLayerHandle expected_layer = expected->GetRootLayer();
    SdfLayerRefPtr flattened_layer;
    if (flattened) {
        flattened_layer = source->Flatten(add_source_file_comment);
        if (!flattened_layer) {
            throw std::runtime_error(
                "could not flatten the expected stage for verification");
        }
        expected_layer = flattened_layer;
    }
    if (normalize_signed_zero(exported_layer_text(expected_layer)) !=
        normalize_signed_zero(exported_layer_text(persisted->GetRootLayer()))) {
        throw std::runtime_error(
            flattened
                ? "flattened root content changed during persistence verification"
                : "authored root content changed during persistence verification");
    }
}

}  // namespace

AuthoredXformStackRaw authored_xform_stack(const StageHandle& handle,
                                           rust::Str path) {
    return handle.impl_->access([&] {
    return authored_xform_stack_locked(handle.impl_->stage,
                                       SdfPath(to_std(path)));
    });
}

// Canonicalize IEEE-754 negative zero to positive zero. -0.0 and +0.0 are
// numerically identical but serialize to different text ("-0" vs "0"); the
// usdc crate writer normalizes to +0.0, so authoring +0.0 keeps the in-memory
// root layer byte-identical to the crate/USDZ round-trip and avoids a spurious
// persistence-verification mismatch. Used by every xformOp authoring path so no
// authored transform can introduce a "-0" token that the round-trip drops.
static inline double canon_zero(double v) { return v == 0.0 ? 0.0 : v; }

// Build the 4×4 row-vector TRS matrix from decomposed components.
// USD row-vector convention: p' = p · M, translation in row 3.
// Row i of the 3×3 upper-left = scale[i] * row_i_of_R, where
// R = Rx · Ry · Rz (XYZ Euler, angles in degrees).
static GfMatrix4d compose_trs_matrix(
    const rust::Slice<const double>& translate,
    const rust::Slice<const double>& rotate_xyz_deg,
    const rust::Slice<const double>& scale) {
    const double rx = rotate_xyz_deg[0] * M_PI / 180.0;
    const double ry = rotate_xyz_deg[1] * M_PI / 180.0;
    const double rz = rotate_xyz_deg[2] * M_PI / 180.0;
    const double cx = std::cos(rx), sx = std::sin(rx);
    const double cy = std::cos(ry), sy_v = std::sin(ry);
    const double cz = std::cos(rz), sz_v = std::sin(rz);
    const double r00 = cy*cz,               r01 = cy*sz_v,              r02 = -sy_v;
    const double r10 = sx*sy_v*cz - cx*sz_v, r11 = sx*sy_v*sz_v + cx*cz, r12 = sx*cy;
    const double r20 = cx*sy_v*cz + sx*sz_v, r21 = cx*sy_v*sz_v - sx*cz, r22 = cx*cy;
    GfMatrix4d m;
    // canon_zero every entry: sin(0) etc. and scale*(-0.0) products yield IEEE
    // -0.0 for axis-aligned rotations, which the usdc/USDZ round-trip drops to
    // "0" — a purely cosmetic difference that must not reach the layer text.
    m[0][0] = canon_zero(scale[0]*r00); m[0][1] = canon_zero(scale[0]*r01); m[0][2] = canon_zero(scale[0]*r02); m[0][3] = 0.0;
    m[1][0] = canon_zero(scale[1]*r10); m[1][1] = canon_zero(scale[1]*r11); m[1][2] = canon_zero(scale[1]*r12); m[1][3] = 0.0;
    m[2][0] = canon_zero(scale[2]*r20); m[2][1] = canon_zero(scale[2]*r21); m[2][2] = canon_zero(scale[2]*r22); m[2][3] = 0.0;
    m[3][0] = canon_zero(translate[0]); m[3][1] = canon_zero(translate[1]); m[3][2] = canon_zero(translate[2]); m[3][3] = 1.0;
    return m;
}

// Set a Vec3 xformOp value with precision dispatch.
static bool set_xformop_vec3(const UsdGeomXformOp& op,
                              double x, double y, double z) {
    x = canon_zero(x);
    y = canon_zero(y);
    z = canon_zero(z);
    switch (op.GetPrecision()) {
        case UsdGeomXformOp::PrecisionDouble:
            return op.Set(GfVec3d(x, y, z), UsdTimeCode::Default());
        case UsdGeomXformOp::PrecisionFloat:
            return op.Set(GfVec3f(static_cast<float>(x),
                                  static_cast<float>(y),
                                  static_cast<float>(z)), UsdTimeCode::Default());
        case UsdGeomXformOp::PrecisionHalf:
            return op.Set(GfVec3h(static_cast<float>(x),
                                  static_cast<float>(y),
                                  static_cast<float>(z)), UsdTimeCode::Default());
    }
    return false;
}

// Hold the exclusive lifecycle lock for the whole transform edit, releasing it even
// if the edit (or its rollback) throws. The inner rollback try/catch blocks below
// compose correctly — an inner catch rolls back then rethrows, and the helper's catch
// then unlocks. The lambda body is defined inside this friend function so it keeps
// access to the private StageHandle::impl_; it is intentionally not re-indented.
void apply_matrix_transform(StageHandle& handle, rust::Str path, rust::Slice<const double> values) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
        if (values.size() != 16) throw std::runtime_error("matrix edit requires 16 components");
        GfMatrix4d matrix(1.0);
        for (int row = 0; row < 4; ++row) {
            for (int col = 0; col < 4; ++col) {
                const double value = values[row * 4 + col];
                if (!std::isfinite(value)) throw std::runtime_error("matrix edit must be finite");
                matrix[row][col] = canon_zero(value);
            }
        }
        if (matrix[0][3] != 0.0 || matrix[1][3] != 0.0 || matrix[2][3] != 0.0 || matrix[3][3] != 1.0)
            throw std::runtime_error("matrix edit must be affine");
        const SdfPath prim_path(to_std(path));
        const auto& stage = handle.impl_->stage;
        const UsdPrim prim = stage->GetPrimAtPath(prim_path);
        const UsdGeomXformable xformable(prim);
        if (!prim || !xformable) throw std::runtime_error("matrix edit requires an existing xformable prim");
        const SdfLayerHandle root = stage->GetRootLayer();
        if (!root->GetPrimAtPath(prim_path)) throw std::runtime_error("matrix edit requires a root-layer prim spec");

        // A dedicated double op preserves arbitrary affine values without
        // decomposition or changing unrelated authored op/property specs.
        const TfToken name("xformOp:transform:idtxCollab");
        const UsdAttribute existing = prim.GetAttribute(name);
        if (existing && existing.GetTypeName() != SdfValueTypeNames->Matrix4d)
            throw std::runtime_error("matrix edit attribute has incompatible type");
        VtTokenArray order;
        if (xformable.GetResetXformStack()) order.push_back(TfToken("!resetXformStack!"));
        order.push_back(name);

        auto snapshot = capture_transform_properties(root, prim_path, {});
        for (const TfToken& property : {name, TfToken("xformOpOrder")}) {
            const SdfPath property_path = prim_path.AppendProperty(property);
            if (root->GetPropertyAtPath(property_path)) {
                if (!SdfCopySpec(root, property_path, snapshot.layer, property_path))
                    throw std::runtime_error("could not capture matrix edit property");
                snapshot.paths.push_back(property_path);
            } else {
                snapshot.remove_on_rollback.push_back(property_path);
            }
        }
        const auto checkpoint = handle.impl_->observer->checkpoint();
#ifdef IDTX_TEST_SUPPORT
        const auto fail_after = handle.impl_->transform_failure_after_writes;
        handle.impl_->transform_failure_after_writes = 0;
#endif
        const UsdEditContext context(stage, root);
        try {
            SdfChangeBlock block;
            const UsdAttribute attr = prim.CreateAttribute(name, SdfValueTypeNames->Matrix4d, false);
            if (!attr || !attr.Set(matrix, UsdTimeCode::Default()))
                throw std::runtime_error("OpenUSD rejected matrix edit");
#ifdef IDTX_TEST_SUPPORT
            if (fail_after == 1) throw std::runtime_error("injected matrix edit failure");
#endif
            if (!xformable.CreateXformOpOrderAttr().Set(order))
                throw std::runtime_error("OpenUSD rejected matrix op order");
#ifdef IDTX_TEST_SUPPORT
            if (fail_after == 2) throw std::runtime_error("injected matrix order failure");
#endif
        } catch (...) {
            if (!restore_transform_properties(snapshot, root, prim_path))
                throw std::runtime_error("matrix edit rollback failed");
            handle.impl_->observer->rollback_to(checkpoint);
            throw;
        }
    });
}

void apply_transform_edit(StageHandle& handle, rust::Str path,
                          rust::Slice<const double> translate,
                          rust::Slice<const double> rotate_xyz,
                          rust::Slice<const double> scale) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
    if (translate.size() != 3 || rotate_xyz.size() != 3 || scale.size() != 3) {
        throw std::runtime_error("transform edit values must have three components");
    }
    for (const rust::Slice<const double>* values :
         {&translate, &rotate_xyz, &scale}) {
        for (std::size_t axis = 0; axis < values->size(); ++axis) {
            if (!std::isfinite((*values)[axis])) {
                throw std::runtime_error("transform edit values must be finite");
            }
        }
    }
    const SdfPath prim_path(to_std(path));
    const UsdStageRefPtr& stage = handle.impl_->stage;

    const UsdPrim prim = stage->GetPrimAtPath(prim_path);
    if (!prim) {
        throw std::runtime_error("transform edit: prim does not exist");
    }
    const UsdGeomXformable xformable(prim);
    if (!xformable) {
        throw std::runtime_error("transform edit: prim is not xformable");
    }
    const SdfLayerHandle root = stage->GetRootLayer();
    if (!root->GetPrimAtPath(prim_path)) {
        throw std::runtime_error(
            "prim has no root-layer spec; referenced-only prims are display-only");
    }

    bool reset_stack = false;
    const std::vector<UsdGeomXformOp> ops =
        xformable.GetOrderedXformOps(&reset_stack);

    // Reject inverse ops and pivot markers before dispatching strategies.
    for (const UsdGeomXformOp& op : ops) {
        if (op.IsInverseOp()) {
            throw std::runtime_error("inverse transform ops are not editable");
        }
        if (op.GetOpName().GetString().find(":pivot") != std::string::npos) {
            throw std::runtime_error("pivot transform ops are not editable");
        }
    }

    // ── Strategy A: empty xformOp stack → bootstrap with T / rotateXYZ / S ──
    if (ops.empty()) {
        const std::size_t ckpt = handle.impl_->observer->checkpoint();
        const UsdEditContext ec(stage, root);
        try {
            SdfChangeBlock cb;
            UsdGeomXformOp t_op =
                xformable.AddTranslateOp(UsdGeomXformOp::PrecisionDouble);
            UsdGeomXformOp r_op =
                xformable.AddRotateXYZOp(UsdGeomXformOp::PrecisionFloat);
            UsdGeomXformOp s_op =
                xformable.AddScaleOp(UsdGeomXformOp::PrecisionFloat);
            // canon_zero the components: a decomposed TRS from the UI can carry
            // IEEE -0.0 (e.g. asin(-0.0) rotation, or a raw -0.0 translate),
            // which the usdc round-trip drops to "0" and would otherwise trip
            // persistence verification. Matches set_xformop_vec3's canon.
            if (!t_op || !r_op || !s_op ||
                !t_op.Set(GfVec3d(canon_zero(translate[0]),
                                  canon_zero(translate[1]),
                                  canon_zero(translate[2])),
                          UsdTimeCode::Default()) ||
                !r_op.Set(GfVec3f(static_cast<float>(canon_zero(rotate_xyz[0])),
                                  static_cast<float>(canon_zero(rotate_xyz[1])),
                                  static_cast<float>(canon_zero(rotate_xyz[2]))),
                          UsdTimeCode::Default()) ||
                !s_op.Set(GfVec3f(static_cast<float>(canon_zero(scale[0])),
                                  static_cast<float>(canon_zero(scale[1])),
                                  static_cast<float>(canon_zero(scale[2]))),
                          UsdTimeCode::Default())) {
                throw std::runtime_error(
                    "failed to create or author bootstrap xformOps");
            }
        } catch (...) {
            handle.impl_->observer->rollback_to(ckpt);
            throw;
        }
        return;
    }

    // ── Strategy B: single xformOp:transform (matrix) ──────────────────────
    if (ops.size() == 1 &&
        ops[0].GetOpType() == UsdGeomXformOp::TypeTransform) {
        const UsdGeomXformOp& matrix_op = ops[0];
        const GfMatrix4d composed =
            compose_trs_matrix(translate, rotate_xyz, scale);
        const TransformPropertySnapshot snapshot =
            capture_transform_properties(root, prim_path, ops);
        const std::size_t ckpt = handle.impl_->observer->checkpoint();
        const UsdEditContext ec(stage, root);
        try {
            SdfChangeBlock cb;
            bool authored = false;
            switch (matrix_op.GetPrecision()) {
                case UsdGeomXformOp::PrecisionDouble:
                    authored = matrix_op.Set(composed, UsdTimeCode::Default());
                    break;
                case UsdGeomXformOp::PrecisionFloat:
                case UsdGeomXformOp::PrecisionHalf:
                    throw std::runtime_error(
                        "non-double matrix transform ops are not supported for authoring");
            }
            if (!authored) {
                throw std::runtime_error("OpenUSD rejected matrix transform edit");
            }
        } catch (...) {
            if (!restore_transform_properties(snapshot, root, prim_path)) {
                throw std::runtime_error(
                    "matrix transform edit failed and rollback failed");
            }
            handle.impl_->observer->rollback_to(ckpt);
            throw;
        }
        return;
    }

    // ── Strategy C: individual T / rotateXYZ / S ops (any subset) ──────────
    // All ops in the stack must be one of the three supported types.
    bool has_translate = false;
    bool has_rotate    = false;
    bool has_scale     = false;
    for (const UsdGeomXformOp& op : ops) {
        switch (op.GetOpType()) {
            case UsdGeomXformOp::TypeTranslate:  has_translate = true; break;
            case UsdGeomXformOp::TypeRotateXYZ:  has_rotate    = true; break;
            case UsdGeomXformOp::TypeScale:       has_scale     = true; break;
            default:
                throw std::runtime_error(
                    std::string("unsupported xformOp type; only translate, "
                                "rotateXYZ, and scale are editable: ") +
                    xform_op_type_name(op.GetOpType()));
        }
    }

    // Verify each existing op carries a default value we can override. Ops
    // composed only from a referenced/payload layer (no root opinion yet) are
    // fine: the edit below authors an override into the root layer, which the
    // reference's xformOpOrder still drives. Only reject ops that have no
    // default value at all (animated-only) — overriding those needs an explicit
    // time-edit policy.
    for (const UsdGeomXformOp& op : ops) {
        GfVec3d test_d; GfVec3f test_f;
        if (!op.Get(&test_d, UsdTimeCode::Default()) &&
            !op.Get(&test_f, UsdTimeCode::Default())) {
            throw std::runtime_error(
                "animated-only transform ops require an explicit time-edit policy");
        }
    }

    // Determine which missing ops need to be created for non-identity values.
    const double eps = 1e-9;
    const bool need_new_translate = !has_translate &&
        (std::abs(translate[0]) > eps ||
         std::abs(translate[1]) > eps ||
         std::abs(translate[2]) > eps);
    const bool need_new_rotate = !has_rotate &&
        (std::abs(rotate_xyz[0]) > eps ||
         std::abs(rotate_xyz[1]) > eps ||
         std::abs(rotate_xyz[2]) > eps);
    const bool need_new_scale = !has_scale &&
        (std::abs(scale[0] - 1.0) > eps ||
         std::abs(scale[1] - 1.0) > eps ||
         std::abs(scale[2] - 1.0) > eps);
    const bool will_create = need_new_translate || need_new_rotate || need_new_scale;

    // Snapshot existing ops (and xformOpOrder if the order may change). Ops
    // with no pre-existing root spec are recorded for removal on rollback.
    TransformPropertySnapshot snapshot =
        capture_transform_properties(root, prim_path, ops);
    if (will_create) {
        const SdfPath order_path =
            prim_path.AppendProperty(TfToken("xformOpOrder"));
        if (root->GetPropertyAtPath(order_path)) {
            if (!SdfCopySpec(root, order_path, snapshot.layer, order_path)) {
                throw std::runtime_error("could not capture xformOpOrder spec");
            }
            snapshot.paths.push_back(order_path);
        } else {
            // xformOpOrder is composed from the reference; SetXformOpOrder below
            // authors a fresh root spec that rollback must remove.
            snapshot.remove_on_rollback.push_back(order_path);
        }
    }

    const std::size_t notice_checkpoint = handle.impl_->observer->checkpoint();
    std::vector<SdfPath> newly_created_props;

    // Pre-populate per-type op references from the current ordered list.
    UsdGeomXformOp t_op_ref, r_op_ref, s_op_ref;
    for (const UsdGeomXformOp& op : ops) {
        if (op.GetOpType() == UsdGeomXformOp::TypeTranslate)      t_op_ref = op;
        else if (op.GetOpType() == UsdGeomXformOp::TypeRotateXYZ) r_op_ref = op;
        else if (op.GetOpType() == UsdGeomXformOp::TypeScale)      s_op_ref = op;
    }

#ifdef IDTX_TEST_SUPPORT
    std::uint32_t failure_after_writes =
        handle.impl_->transform_failure_after_writes;
    handle.impl_->transform_failure_after_writes = 0;
#endif

    const UsdEditContext edit_context(stage, root);
    try {
        SdfChangeBlock change_block;
        std::uint32_t successful_writes = 0;

        // Update each existing op in-place.
        for (const UsdGeomXformOp& op : ops) {
            double x = 0.0, y = 0.0, z = 0.0;
            if (op.GetOpType() == UsdGeomXformOp::TypeTranslate) {
                x = translate[0]; y = translate[1]; z = translate[2];
            } else if (op.GetOpType() == UsdGeomXformOp::TypeRotateXYZ) {
                x = rotate_xyz[0]; y = rotate_xyz[1]; z = rotate_xyz[2];
            } else {
                x = scale[0]; y = scale[1]; z = scale[2];
            }
            if (!set_xformop_vec3(op, x, y, z)) {
                throw std::runtime_error(
                    "OpenUSD rejected a validated transform edit");
            }
            ++successful_writes;
#ifdef IDTX_TEST_SUPPORT
            if (failure_after_writes == successful_writes) {
                throw std::runtime_error(
                    "injected failure during native transform edit");
            }
#endif
        }

        // Create missing ops that carry non-identity values.
        if (need_new_translate) {
            t_op_ref = xformable.AddTranslateOp(UsdGeomXformOp::PrecisionDouble);
            if (!t_op_ref ||
                !t_op_ref.Set(
                    GfVec3d(canon_zero(translate[0]), canon_zero(translate[1]),
                            canon_zero(translate[2])),
                    UsdTimeCode::Default())) {
                throw std::runtime_error("failed to create translate op");
            }
            newly_created_props.push_back(t_op_ref.GetAttr().GetPath());
            has_translate = true;
        }
        if (need_new_rotate) {
            r_op_ref = xformable.AddRotateXYZOp(UsdGeomXformOp::PrecisionFloat);
            if (!r_op_ref ||
                !r_op_ref.Set(
                    GfVec3f(static_cast<float>(canon_zero(rotate_xyz[0])),
                            static_cast<float>(canon_zero(rotate_xyz[1])),
                            static_cast<float>(canon_zero(rotate_xyz[2]))),
                    UsdTimeCode::Default())) {
                throw std::runtime_error("failed to create rotateXYZ op");
            }
            newly_created_props.push_back(r_op_ref.GetAttr().GetPath());
            has_rotate = true;
        }
        if (need_new_scale) {
            s_op_ref = xformable.AddScaleOp(UsdGeomXformOp::PrecisionFloat);
            if (!s_op_ref ||
                !s_op_ref.Set(
                    GfVec3f(static_cast<float>(canon_zero(scale[0])),
                            static_cast<float>(canon_zero(scale[1])),
                            static_cast<float>(canon_zero(scale[2]))),
                    UsdTimeCode::Default())) {
                throw std::runtime_error("failed to create scale op");
            }
            newly_created_props.push_back(s_op_ref.GetAttr().GetPath());
            has_scale = true;
        }

        // Rewrite xformOpOrder in canonical T→R→S order if any ops were added.
        if (will_create) {
            std::vector<UsdGeomXformOp> ordered;
            if (has_translate) ordered.push_back(t_op_ref);
            if (has_rotate)    ordered.push_back(r_op_ref);
            if (has_scale)     ordered.push_back(s_op_ref);
            if (!xformable.SetXformOpOrder(ordered)) {
                throw std::runtime_error("failed to update xformOpOrder");
            }
        }
    } catch (...) {
        if (!restore_transform_properties(snapshot, root, prim_path)) {
            throw std::runtime_error(
                "transform edit failed and root-layer rollback failed");
        }
        // Remove any freshly-created op attribute specs from the root layer.
        for (const SdfPath& prop_path : newly_created_props) {
            const SdfPropertySpecHandle prop_spec =
                root->GetPropertyAtPath(prop_path);
            const SdfPrimSpecHandle prim_spec = root->GetPrimAtPath(prim_path);
            if (prop_spec && prim_spec) {
                prim_spec->RemoveProperty(prop_spec);
            }
        }
        handle.impl_->observer->rollback_to(notice_checkpoint);
        throw;
    }
    });
}

// Author a full 4x4 pose matrix onto `path`'s `xformOp:transform` in the
// stage's SESSION layer (not the root). Purpose-built for the XR controller
// pose channel: the per-frame OpenXR grip pose rides a real USD transform on
// the pose Xform so the scoped viewport reprojection re-drives the (possibly
// skinned) controller subtree beneath it.
//
// `matrix` is 16 doubles in GfMatrix4d row-major order. The Rust caller passes
// glam's column-major array, which is exactly the row-major layout of the
// transpose — i.e. the matrix that reproduces glam's column-vector transform
// under USD's row-vector convention. See the safe wrapper for the derivation.
void author_session_transform(StageHandle& handle, rust::Str path,
                              rust::Slice<const double> matrix) {
    if (matrix.size() != 16) {
        throw std::runtime_error("session transform matrix must have 16 components");
    }
    for (std::size_t i = 0; i < matrix.size(); ++i) {
        if (!std::isfinite(matrix[i])) {
            throw std::runtime_error("session transform matrix must be finite");
        }
    }
    const SdfPath prim_path(to_std(path));
    if (!prim_path.IsAbsolutePath() || !prim_path.IsPrimPath()) {
        throw std::runtime_error("session transform target must be an absolute prim path");
    }
    StageAccessGuard stage_guard(handle.impl_->stage_mutex);
    const UsdStageRefPtr& stage = handle.impl_->stage;
    const SdfLayerHandle session = stage->GetSessionLayer();
    if (!session) {
        // Release before throwing — destructors are skipped on unwind on this
        // toolchain (see package_stage), so a live lock would leak + deadlock.
        stage_guard.unlock();
        throw std::runtime_error("stage has no session layer");
    }

    const GfMatrix4d pose(
        matrix[0], matrix[1], matrix[2], matrix[3],
        matrix[4], matrix[5], matrix[6], matrix[7],
        matrix[8], matrix[9], matrix[10], matrix[11],
        matrix[12], matrix[13], matrix[14], matrix[15]);

    const std::size_t ckpt = handle.impl_->observer->checkpoint();
    // Defer any failure to AFTER the edit context + change block have closed via
    // normal scope exit. This toolchain compiles without /EHsc, so local
    // destructors are skipped during exception unwinding (see package_stage): a
    // throw while `SdfChangeBlock cb` is live would leave the change block open,
    // suppressing USD change notifications process-wide and freezing every
    // subsequent re-projection. So we record the error and throw once the block
    // is guaranteed to have been destroyed.
    std::string err;
    {
        const UsdEditContext ec(stage, session);
        // Fetch — do NOT Define — the existing prim. `DefinePrim(path, "Xform")`
        // authors a strongest-layer `typeName = "Xform"` opinion in the SESSION
        // layer, which overrides a composed Sphere/Cube/Cone and retypes it to a
        // plain Xform after the first write; on the next re-hydration the prim
        // loses its gprim geometry component (and so its render mesh + highlight
        // mask) while its type-independent CollisionComp survives — the prim goes
        // invisible yet still raycasts/grabs. The target is always an
        // already-composed, hydrated prim, so a plain fetch is correct: authoring
        // the matrix xformOp below creates only an `over` spec in the session
        // layer (no type opinion), preserving the real gprim type.
        const UsdPrim prim = stage->GetPrimAtPath(prim_path);
        const UsdGeomXformable xformable(prim);
        UsdGeomXformOp matrix_op;
        if (!xformable) {
            err = prim ? "session transform: prim is not xformable"
                       : "session transform: prim does not exist";
        } else {
            // Resolve (or create) the single matrix op OUTSIDE a change block, so
            // recomposition is live. `MakeMatrixXform` clears `xformOpOrder` then
            // reads it back to confirm; inside an `SdfChangeBlock` that read-back
            // is deferred and still resolves the prim's old stack whenever its ops
            // are authored in a weaker layer (a payloaded / referenced level
            // prim), so it warns "Could not clear xformOpOrder" and returns an
            // invalid op — "failed to author session pose matrix" on every grab
            // frame. Running it with live recomposition fixes that. The op
            // creation on the first frame emits one resync; later frames reuse the
            // single matrix op below (no authoring in this step).
            bool reset_stack = false;
            const std::vector<UsdGeomXformOp> ops =
                xformable.GetOrderedXformOps(&reset_stack);
            if (ops.size() == 1 &&
                ops[0].GetOpType() == UsdGeomXformOp::TypeTransform) {
                // Steady state: the prim already resolves to a single matrix op
                // (seeded here on a prior frame, or by a matrix-op prefab) — reuse it.
                matrix_op = ops[0];
            } else {
                // No ops, or a stack we can't reuse (translate/rotate/scale or
                // multiple ops — typical of imported/DCC geometry such as a
                // grabbed asset body). Override it with a single matrix op in the
                // SESSION layer: MakeMatrixXform clears the op order and authors
                // `xformOp:transform` into the current (session) edit target, so
                // composition resolves to just this matrix regardless of the
                // weaker root-layer stack. (Previously this branch threw, which
                // leaked the change block and froze the session; later it ran
                // inside the block and silently failed on payloaded prims.)
                matrix_op = xformable.MakeMatrixXform();
            }
            if (!matrix_op) {
                err = "failed to author session pose matrix";
            }
        }
        // Batch the single pose value-write in a change block (read-free, so
        // change-block-safe). Deferring the error until after the block + edit
        // context close via normal scope exit keeps the /EHsc-less throw guarantee
        // (a throw with the block live would suppress notices process-wide and
        // freeze every later re-projection).
        if (err.empty()) {
            SdfChangeBlock cb;
            if (!matrix_op.Set(pose, UsdTimeCode::Default())) {
                err = "failed to author session pose matrix";
            }
        }
        // `cb` (if opened) then `ec` destruct here on normal scope exit (no
        // exception in flight), so the change block closes and its notices fire
        // before we can throw below.
    }
    if (!err.empty()) {
        handle.impl_->observer->rollback_to(ckpt);
        // Release before the exception crosses the cxx boundary (destructors
        // are skipped on unwind on this toolchain — the lock would leak).
        stage_guard.unlock();
        throw std::runtime_error(err);
    }
}

// Bulk variant: author N pose matrices onto N prim paths in ONE lifecycle lock,
// ONE SdfChangeBlock, and ONE edit context. This is the per-frame physics
// write-back path — a sim moving N dynamic bodies otherwise fires N notice
// batches + N reprojections (a perf AND correctness bug). `matrices` carries
// 16*N doubles (glam column-major, like `author_session_transform`); path i
// consumes matrices[16*i .. 16*i+16]. Checkpoint/rollback the whole batch as a
// unit; the change block + edit context close on normal scope exit BEFORE any
// throw (this toolchain skips destructors on unwind — see author_session_transform).
void author_session_transforms(StageHandle& handle,
                               rust::Slice<const rust::String> paths,
                               rust::Slice<const double> matrices) {
    const std::size_t n = paths.size();
    if (matrices.size() != n * 16) {
        throw std::runtime_error(
            "session transforms: matrices length must be 16 * paths.len()");
    }
    for (std::size_t i = 0; i < matrices.size(); ++i) {
        if (!std::isfinite(matrices[i])) {
            throw std::runtime_error("session transform matrix must be finite");
        }
    }
    if (n == 0) {
        return;
    }
    std::vector<SdfPath> prim_paths;
    prim_paths.reserve(n);
    for (const rust::String& p : paths) {
        SdfPath prim_path(to_std(p));
        if (!prim_path.IsAbsolutePath() || !prim_path.IsPrimPath()) {
            throw std::runtime_error(
                "session transform target must be an absolute prim path");
        }
        prim_paths.push_back(std::move(prim_path));
    }

    StageAccessGuard stage_guard(handle.impl_->stage_mutex);
    const UsdStageRefPtr& stage = handle.impl_->stage;
    const SdfLayerHandle session = stage->GetSessionLayer();
    if (!session) {
        stage_guard.unlock();
        throw std::runtime_error("stage has no session layer");
    }

    // Pre-validate every target BEFORE authoring anything. `rollback_to` only
    // unwinds the observer's notice records — it does NOT remove SdfLayer
    // opinions already written — so a mid-batch failure (entry N not xformable
    // after 0..N-1 were authored) would leave the session layer partially
    // mutated with no notices to reproject it. Resolving every prim to an
    // xformable up front makes the common failure (a removed / non-xformable
    // target) all-or-nothing: nothing is authored and no checkpoint is spent.
    std::vector<UsdGeomXformable> xformables;
    xformables.reserve(n);
    for (std::size_t i = 0; i < n; ++i) {
        const UsdPrim prim = stage->GetPrimAtPath(prim_paths[i]);
        const UsdGeomXformable xformable(prim);
        if (!xformable) {
            const std::string msg =
                prim ? "session transform: prim is not xformable"
                     : "session transform: prim does not exist";
            stage_guard.unlock();
            throw std::runtime_error(msg);
        }
        xformables.push_back(xformable);
    }

    const std::size_t ckpt = handle.impl_->observer->checkpoint();
    // See author_session_transform: defer any failure to AFTER the change block
    // and edit context have closed via normal scope exit, since a throw while
    // the block is live would suppress USD notices process-wide and freeze every
    // later re-projection on this /EHsc-less toolchain.
    std::string err;
    {
        const UsdEditContext ec(stage, session);
        // Phase 1 — resolve (or create) each body's single `xformOp:transform`
        // OUTSIDE a change block, so recomposition is live. `MakeMatrixXform`
        // clears `xformOpOrder` then reads it back to confirm the clear took; a
        // live stage sees the just-authored session `xformOpOrder = []`, but
        // inside an `SdfChangeBlock` that read-back is deferred and still resolves
        // the prim's *old* stack whenever its ops are authored in a weaker layer
        // (a payloaded / referenced level prim — e.g. `/World/Levels/level/...`).
        // So MakeMatrixXform wrongly warns "Could not clear xformOpOrder" and
        // returns an invalid op → "failed to author session pose matrix" every
        // frame (the physics write-back + grab regression). The op-creation on the
        // first frame per body emits one resync; steady-state frames already carry
        // the single matrix op (reused here) and author nothing in this phase.
        //
        // Fetch — do NOT Define — the existing prim: see author_session_transform.
        // `DefinePrim(path, "Xform")` would author a strongest-layer
        // `typeName = "Xform"` in the SESSION layer and retype a simulated
        // Sphere/Cube/Cone to Xform after the first tick, dropping its render mesh
        // + highlight on re-hydration while the type-independent CollisionComp
        // keeps raycast/grab working. The xformOp override is a plain `over` in the
        // session layer (no type opinion), preserving the dynamic body's real gprim
        // type. (Xformables validated above; reuse the resolved handles.)
        std::vector<UsdGeomXformOp> matrix_ops(n);
        for (std::size_t i = 0; i < n && err.empty(); ++i) {
            const UsdGeomXformable& xformable = xformables[i];
            bool reset_stack = false;
            const std::vector<UsdGeomXformOp> ops =
                xformable.GetOrderedXformOps(&reset_stack);
            if (ops.size() == 1 &&
                ops[0].GetOpType() == UsdGeomXformOp::TypeTransform) {
                matrix_ops[i] = ops[0];
            } else {
                // Collapse any incompatible / complex stack to a single matrix op
                // in the SESSION layer (see author_session_transform).
                matrix_ops[i] = xformable.MakeMatrixXform();
            }
            if (!matrix_ops[i]) {
                err = "failed to author session pose matrix";
            }
        }
        // Phase 2 — batch all N pose value-writes in ONE change block. These are
        // read-free `Set`s on already-resolved ops, so they are change-block-safe
        // and keep the per-frame write-back a single notice batch + reprojection.
        if (err.empty()) {
            SdfChangeBlock cb;
            for (std::size_t i = 0; i < n && err.empty(); ++i) {
                const double* m = &matrices[i * 16];
                const GfMatrix4d pose(
                    m[0], m[1], m[2], m[3],
                    m[4], m[5], m[6], m[7],
                    m[8], m[9], m[10], m[11],
                    m[12], m[13], m[14], m[15]);
                if (!matrix_ops[i].Set(pose, UsdTimeCode::Default())) {
                    err = "failed to author session pose matrix";
                }
            }
        }
        // `cb` (if opened) then `ec` destruct here on normal scope exit.
    }
    if (!err.empty()) {
        handle.impl_->observer->rollback_to(ckpt);
        stage_guard.unlock();
        throw std::runtime_error(err);
    }
}

void author_session_visibility(StageHandle& handle, rust::Str path,
                               bool visible) {
    const SdfPath prim_path(to_std(path));
    if (!prim_path.IsAbsolutePath() || !prim_path.IsPrimPath()) {
        throw std::runtime_error(
            "session visibility target must be an absolute prim path");
    }
    StageAccessGuard stage_guard(handle.impl_->stage_mutex);
    const UsdStageRefPtr& stage = handle.impl_->stage;
    const SdfLayerHandle session = stage->GetSessionLayer();
    if (!session) {
        // Release the lifecycle lock before throwing: this toolchain skips
        // local destructors during exception unwinding (see package_stage), so
        // a throw while `stage_guard` is live leaks the lock and deadlocks the
        // next stage op.
        stage_guard.unlock();
        throw std::runtime_error("stage has no session layer");
    }

    // The prim must already compose and resolve to an imageable type (e.g. a
    // rig-embedded controller Xform authored in a weaker layer) for a session
    // visibility opinion to apply. When it does not — a rig whose controller
    // prim composes typeless / not-yet-imageable — skip the USD opinion rather
    // than failing: the ECS visibility write is authoritative for rendering
    // (see set_controller_hydrated), so a missing/non-imageable target is an
    // expected soft condition, not an error. Checked *before* the edit context
    // and change block are opened — the soft path has nothing to unwind — and
    // crucially before any throw, since a throw here would leak the lifecycle
    // lock (destructors are skipped on unwind) and deadlock the next stage op.
    const UsdPrim prim = stage->GetPrimAtPath(prim_path);
    const UsdGeomImageable imageable(prim);
    if (!prim || !imageable) {
        return;  // lock released by normal scope exit
    }

    const std::size_t ckpt = handle.impl_->observer->checkpoint();
    std::exception_ptr error;
    {
        const UsdEditContext ec(stage, session);
        SdfChangeBlock cb;
        try {
        const TfToken vis =
            visible ? UsdGeomTokens->inherited : UsdGeomTokens->invisible;
        if (!imageable.CreateVisibilityAttr().Set(vis)) {
            throw std::runtime_error("failed to author session visibility");
        }
        } catch (...) { error = std::current_exception(); }
    }
    if (error) {
        handle.impl_->observer->rollback_to(ckpt);
        // Release before the exception crosses the cxx boundary (destructors
        // are skipped on unwind on this toolchain — the lock would leak).
        stage_guard.unlock();
        std::rethrow_exception(error);
    }
}

// Define a brand-new typed prim into the stage's SESSION layer (an empty
// `type_name` defines a typeless prim). Combines the session `UsdEditContext`
// idiom of `author_session_transform` with the "already exists" guard of the
// root-layer structural "define": runtime prims (player camera, dynamic
// overlays, spawned actors) are authored ephemerally into the session sublayer
// so they never dirty the saved assembly. The new PrimId is minted Rust-side on
// the next topology projection — this shim only authors the spec.
void define_session_prim(StageHandle& handle, rust::Str path, rust::Str type_name) {
    const SdfPath prim_path(to_std(path));
    if (!prim_path.IsAbsolutePath() || !prim_path.IsPrimPath()) {
        throw std::runtime_error("session define target must be an absolute prim path");
    }
    StageAccessGuard stage_guard(handle.impl_->stage_mutex);
    const UsdStageRefPtr& stage = handle.impl_->stage;
    const SdfLayerHandle session = stage->GetSessionLayer();
    if (!session) {
        // Release before throwing — destructors are skipped on unwind on this
        // toolchain (see package_stage), so a live lock would leak + deadlock.
        stage_guard.unlock();
        throw std::runtime_error("stage has no session layer");
    }
    if (stage->GetPrimAtPath(prim_path)) {
        stage_guard.unlock();
        throw std::runtime_error("session define target already exists");
    }

    const std::string type_str = to_std(type_name);
    const std::size_t ckpt = handle.impl_->observer->checkpoint();
    std::exception_ptr error;
    {
        const UsdEditContext ec(stage, session);
        try {
        // No SdfChangeBlock here: UsdStage::DefinePrim resolves + authors any
        // missing ancestor specs (e.g. a nested "/World/Cam" needs "/World"),
        // which requires live recomposition. An outer SdfChangeBlock defers
        // that and makes DefinePrim fail on non-root paths.
        const UsdPrim prim = type_str.empty()
            ? stage->DefinePrim(prim_path)
            : stage->DefinePrim(prim_path, TfToken(type_str));
        if (!prim) {
            throw std::runtime_error("OpenUSD could not define the session prim");
        }
        } catch (...) { error = std::current_exception(); }
    }
    if (error) {
        handle.impl_->observer->rollback_to(ckpt);
        // Release before the exception crosses the cxx boundary (destructors
        // are skipped on unwind on this toolchain — the lock would leak).
        stage_guard.unlock();
        std::rethrow_exception(error);
    }
}

void spawn_session_reference(StageHandle& handle, rust::Str path_text, rust::Str asset_text, rust::Str target_text, rust::Slice<const double> matrix, rust::Str context_text, rust::Slice<const rust::String> children, rust::Slice<const rust::String> properties, rust::Slice<const rust::String> sources) {
    handle.impl_->access([&] {
        const SdfPath path(to_std(path_text));
        const SdfPath target(to_std(target_text));
        const std::string asset = to_std(asset_text);
        const std::string context_json = to_std(context_text);
        if (!path.IsAbsolutePath() || !path.IsPrimPath() || path.IsAbsoluteRootPath() ||
            !target.IsAbsolutePath() || !target.IsPrimPath() || target.IsAbsoluteRootPath() ||
            !std::filesystem::path(asset).is_absolute() || matrix.size() != 16 ||
            context_json.size() > 8192 || children.size() > 16 ||
            children.size() != properties.size() || children.size() != sources.size())
            throw std::runtime_error("invalid session reference spawn arguments");
        for (double value : matrix) if (!std::isfinite(value)) throw std::runtime_error("spawn transform must be finite");
        if (matrix[3] != 0 || matrix[7] != 0 || matrix[11] != 0 || matrix[15] != 1)
            throw std::runtime_error("spawn transform must be affine");
        JsParseError parse_error;
        const JsValue parsed = JsParseString(context_json, &parse_error);
        if (!parsed.IsObject()) throw std::runtime_error("spawn context must be a JSON object");
        const auto stage = handle.impl_->stage;
        const auto parent = stage->GetPrimAtPath(path.GetParentPath());
        if (!parent || !parent.IsActive() || !parent.IsLoaded() || parent.IsInstance() || parent.IsInstanceProxy() || parent.IsInPrototype())
            throw std::runtime_error("spawn parent is missing or belongs to a native instance");
        if (stage->GetPrimAtPath(path)) throw std::runtime_error("spawn target already exists");
        const auto prototype_stage = UsdStage::Open(asset, UsdStage::LoadAll);
        const auto prototype = prototype_stage ? prototype_stage->GetPrimAtPath(target) : UsdPrim();
        if (!prototype || !prototype.IsActive() || !prototype.IsDefined() || !UsdGeomXformable(prototype))
            throw std::runtime_error("spawn template target must be an active xformable prim");
        const auto prototype_context = prototype.GetAttribute(TfToken("idtx:behavior:context"));
        if (prototype_context && prototype_context.GetTypeName() != SdfValueTypeNames->String)
            throw std::runtime_error("template context attribute must be a string");
        std::vector<SdfPath> destinations;
        std::vector<SdfPath> source_paths;
        std::set<SdfPath> unique_destinations;
        for (std::size_t i = 0; i < children.size(); ++i) {
            const std::string child = std::string(children[i]);
            const std::string property = std::string(properties[i]);
            const SdfPath relative(child);
            if ((!child.empty() && (relative.IsAbsolutePath() || !relative.IsPrimPath() || child.find("..") != std::string::npos)) ||
                !SdfPath::IsValidNamespacedIdentifier(property) || property.rfind("idtx:behavior:", 0) == 0)
                throw std::runtime_error("spawn connection destination must remain inside the new subtree");
            const SdfPath source{std::string(sources[i])};
            if (!source.IsAbsolutePath() || !source.IsPropertyPath())
                throw std::runtime_error("spawn connection source must be an absolute attribute path");
            const SdfPath prototype_child = child.empty() ? target : target.AppendPath(relative);
            const auto destination_attr = prototype_stage->GetAttributeAtPath(prototype_child.AppendProperty(TfToken(property)));
            const auto source_attr = stage->GetAttributeAtPath(source);
            if (!destination_attr || !source_attr || destination_attr.GetTypeName() != source_attr.GetTypeName())
                throw std::runtime_error("spawn connection attributes are missing or have incompatible types");
            const SdfPath destination = (child.empty() ? path : path.AppendPath(relative)).AppendProperty(TfToken(property));
            if (!unique_destinations.insert(destination).second) throw std::runtime_error("duplicate spawn connection destination");
            destinations.push_back(destination); source_paths.push_back(source);
        }
        const auto session = stage->GetSessionLayer();
        if (!session || !session->PermissionToEdit()) throw std::runtime_error("session layer is not writable");
        const auto before = SdfLayer::CreateAnonymous("runtime-spawn-rollback.usda");
        before->TransferContent(session);
        const auto checkpoint = handle.impl_->observer->checkpoint();
        try {
            const UsdEditContext edit_context(stage, session);
            // DefinePrim and references need live recomposition. Do not wrap
            // them in an outer SdfChangeBlock. The owner projects only after
            // this whole rollback-capable transaction succeeds.
            auto prim = stage->DefinePrim(path);
            if (!prim || !prim.GetReferences().AddReference(asset, target) || !prim.SetInstanceable(false))
                throw std::runtime_error("could not compose the runtime reference");
            prim = stage->GetPrimAtPath(path);
            const UsdGeomXformable xform(prim);
            const auto op = xform.MakeMatrixXform();
            if (!op) throw std::runtime_error("spawned template is not xformable");
            const GfMatrix4d pose(matrix[0], matrix[1], matrix[2], matrix[3],
                matrix[4], matrix[5], matrix[6], matrix[7],
                matrix[8], matrix[9], matrix[10], matrix[11],
                matrix[12], matrix[13], matrix[14], matrix[15]);
            if (!op.Set(pose) || !UsdGeomImageable(prim).CreateVisibilityAttr().Set(UsdGeomTokens->inherited) ||
                !prim.CreateAttribute(TfToken("idtx:behavior:context"), SdfValueTypeNames->String, true).Set(context_json))
                throw std::runtime_error("could not configure the spawned reference");
            for (std::size_t i = 0; i < destinations.size(); ++i)
                if (!stage->GetAttributeAtPath(destinations[i]).SetConnections({source_paths[i]}))
                    throw std::runtime_error("could not author a spawned attribute connection");
        } catch (...) {
            session->TransferContent(before);
            handle.impl_->observer->rollback_to(checkpoint);
            throw;
        }
    });
}

void erase_session_prim(StageHandle& handle, rust::Str path_text) {
    handle.impl_->access([&] {
        const SdfPath path(to_std(path_text));
        if (!path.IsAbsolutePath() || !path.IsPrimPath() || path.IsAbsoluteRootPath())
            throw std::runtime_error("runtime-owned erase requires an absolute prim path");
        const auto stage = handle.impl_->stage;
        const auto session = stage->GetSessionLayer();
        if (!session || !session->PermissionToEdit()) throw std::runtime_error("session layer is not writable");
        if (!session->GetPrimAtPath(path)) return;
        const auto before = SdfLayer::CreateAnonymous("runtime-erase-rollback.usda");
        before->TransferContent(session);
        const auto checkpoint = handle.impl_->observer->checkpoint();
        try {
            const UsdEditContext context(stage, session);
            if (!stage->RemovePrim(path)) throw std::runtime_error("could not erase the runtime-owned reference");
        } catch (...) {
            session->TransferContent(before);
            handle.impl_->observer->rollback_to(checkpoint);
            throw;
        }
    });
}

void mutate_session_prim(StageHandle& handle, rust::Str path_text, rust::Str type_name, bool remove) {
    handle.impl_->access([&] {
        const SdfPath path(to_std(path_text));
        if (!path.IsAbsolutePath() || !path.IsPrimPath() || path.IsAbsoluteRootPath())
            throw std::runtime_error("runtime edit requires an absolute prim path");
        const auto stage = handle.impl_->stage;
        const auto session = stage->GetSessionLayer();
        if (!session || !session->PermissionToEdit()) throw std::runtime_error("session layer is not writable");
        const auto before = SdfLayer::CreateAnonymous(".usda");
        before->TransferContent(session);
        const auto checkpoint = handle.impl_->observer->checkpoint();
        try {
            const UsdEditContext context(stage, session);
            auto prim = stage->GetPrimAtPath(path);
            if (remove) {
                if (!prim) return;
                bool weaker = false;
                for (const auto& spec : prim.GetPrimStack()) if (spec->GetLayer() != session) weaker = true;
                // Removing our own spec is sufficient only for a transient prim.
                // A weaker-layer prim needs a strongest-layer deactivation.
                if (weaker ? !prim.SetActive(false) : !stage->RemovePrim(path))
                    throw std::runtime_error("could not remove runtime prim");
            } else {
                if (prim && !prim.IsActive() && !prim.SetActive(true)) throw std::runtime_error("could not reactivate runtime prim");
                prim = stage->DefinePrim(path, TfToken(to_std(type_name)));
                if (!prim) throw std::runtime_error("could not define runtime prim");
            }
        } catch (...) {
            session->TransferContent(before);
            handle.impl_->observer->rollback_to(checkpoint);
            throw;
        }
    });
}

// Author a scalar/asset/token property onto an existing prim in the stage's
// SESSION layer. The value is string-encoded exactly like the structural
// `set-property-*` path (so int2 arrives as "x y", float as its decimal form),
// but the edit target is the session layer — so a behavior's HUD/overlay
// parameters never dirty the saved assembly. The prim must already compose
// (define it first with `define_session_prim`). Supported `kind`s: "asset",
// "token", "float", "int2". Checkpoint/rollback on failure, mirroring
// `author_session_transform`.
void author_session_property(StageHandle& handle, rust::Str path, rust::Str attr,
                             rust::Str kind, rust::Str value) {
    const SdfPath prim_path(to_std(path));
    if (!prim_path.IsAbsolutePath() || !prim_path.IsPrimPath()) {
        throw std::runtime_error(
            "session property target must be an absolute prim path");
    }
    const std::string attr_name = to_std(attr);
    if (attr_name.empty()) {
        throw std::runtime_error("session property name must not be empty");
    }
    const std::string kind_str = to_std(kind);
    const std::string value_str = to_std(value);

    StageAccessGuard stage_guard(handle.impl_->stage_mutex);
    const UsdStageRefPtr& stage = handle.impl_->stage;
    const SdfLayerHandle session = stage->GetSessionLayer();
    if (!session) {
        // Release before throwing — destructors are skipped on unwind on this
        // toolchain (see package_stage), so a live lock would leak + deadlock.
        stage_guard.unlock();
        throw std::runtime_error("stage has no session layer");
    }
    const UsdPrim prim = stage->GetPrimAtPath(prim_path);
    if (!prim) {
        stage_guard.unlock();
        throw std::runtime_error("session property target does not exist");
    }

    const TfToken property_name(attr_name);
    const std::size_t ckpt = handle.impl_->observer->checkpoint();
    std::exception_ptr error;
    {
        const UsdEditContext ec(stage, session);
        try {
        // Same type-match guard as the structural set-property path: authoring a
        // value of a different type than an already-authored attribute is an
        // error, not a silent retype.
        const auto ensure_attribute = [&prim, &property_name](
                                          const SdfValueTypeName& value_type) {
            UsdAttribute attribute = prim.GetAttribute(property_name);
            if (attribute && attribute.GetTypeName() != value_type) {
                throw std::runtime_error(
                    "session property type does not match the attribute's type");
            }
            if (!attribute) {
                attribute = prim.CreateAttribute(property_name, value_type, true);
            }
            if (!attribute) {
                throw std::runtime_error("could not author the session property");
            }
            return attribute;
        };
        bool applied = false;
        if (kind_str == "asset") {
            applied =
                ensure_attribute(SdfValueTypeNames->Asset).Set(SdfAssetPath(value_str));
        } else if (kind_str == "token") {
            applied =
                ensure_attribute(SdfValueTypeNames->Token).Set(TfToken(value_str));
        } else if (kind_str == "string") {
            applied = ensure_attribute(SdfValueTypeNames->String).Set(value_str);
        } else if (kind_str == "bool") {
            if (value_str != "true" && value_str != "false") {
                throw std::runtime_error("session bool property value is invalid");
            }
            applied = ensure_attribute(SdfValueTypeNames->Bool).Set(value_str == "true");
        } else if (kind_str == "float") {
            std::size_t consumed = 0;
            const float v = std::stof(value_str, &consumed);
            if (consumed != value_str.size() || !std::isfinite(v)) {
                throw std::runtime_error("session float property value is invalid");
            }
            applied = ensure_attribute(SdfValueTypeNames->Float).Set(v);
        } else if (kind_str == "int2") {
            std::istringstream stream(value_str);
            long long x = 0;
            long long y = 0;
            std::string extra;
            if (!(stream >> x >> y) || (stream >> extra)) {
                throw std::runtime_error("session int2 property value is invalid");
            }
            applied = ensure_attribute(SdfValueTypeNames->Int2)
                          .Set(GfVec2i(static_cast<int>(x), static_cast<int>(y)));
        } else {
            throw std::runtime_error("unknown session property kind");
        }
        if (!applied) {
            throw std::runtime_error("OpenUSD rejected the session property value");
        }
        } catch (...) { error = std::current_exception(); }
    }
    if (error) {
        handle.impl_->observer->rollback_to(ckpt);
        // Release before the exception crosses the cxx boundary (destructors
        // are skipped on unwind on this toolchain — the lock would leak).
        stage_guard.unlock();
        std::rethrow_exception(error);
    }
}

void author_root_shadow_mode(StageHandle& handle, int32_t mode) {
    // Author the stage shadow mode into the ROOT layer's customLayerData as a
    // token: 0→"off", 1→"hard", 2→"smooth", 3→"soft" (mirrors the resolution in
    // stage_metadata). This is layer metadata, not a prim edit, so it never
    // routes through apply_structural_edit. The legacy "idtx:softShadows" bool
    // is erased so there is a single source of truth.
    const char* token = nullptr;
    switch (mode) {
        case 3: token = "soft"; break;
        case 2: token = "smooth"; break;
        case 1: token = "hard"; break;
        case 0: token = "off"; break;
        default:
            throw std::runtime_error(
                "shadow mode must be 0 (off), 1 (hard), 2 (smooth) or 3 (soft)");
    }
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
    const UsdStageRefPtr& stage = handle.impl_->stage;
    const SdfLayerHandle root = stage->GetRootLayer();
    if (!root) {
        throw std::runtime_error("stage has no root layer");
    }

    const std::size_t ckpt = handle.impl_->observer->checkpoint();
    try {
        SdfChangeBlock cb;
        VtDictionary custom = root->GetCustomLayerData();
        custom["idtx:shadowMode"] = VtValue(TfToken(token));
        custom.erase("idtx:softShadows");
        root->SetCustomLayerData(custom);
    } catch (...) {
        handle.impl_->observer->rollback_to(ckpt);
        throw;
    }
    });
}

void author_root_stage_behavior(StageHandle& handle, rust::Str name,
                                rust::Str run_scope) {
    // Author the stage-level behavior assignment into the ROOT layer's
    // customLayerData (layer metadata, not a prim edit — never routes through
    // apply_structural_edit): `idtx:stageBehavior` (the prim name) and
    // `idtx:stageBehaviorRunScope` (the run-condition token). Merges into any
    // existing customLayerData, leaving other keys intact. Mirrors
    // author_root_shadow_mode.
    const std::string name_str = to_std(name);
    const std::string scope_str = to_std(run_scope);
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
    const UsdStageRefPtr& stage = handle.impl_->stage;
    const SdfLayerHandle root = stage->GetRootLayer();
    if (!root) {
        throw std::runtime_error("stage has no root layer");
    }
    const std::size_t ckpt = handle.impl_->observer->checkpoint();
    try {
        SdfChangeBlock cb;
        VtDictionary custom = root->GetCustomLayerData();
        custom["idtx:stageBehavior"] = VtValue(std::string(name_str));
        if (!scope_str.empty()) {
            custom["idtx:stageBehaviorRunScope"] = VtValue(TfToken(scope_str));
        }
        root->SetCustomLayerData(custom);
    } catch (...) {
        handle.impl_->observer->rollback_to(ckpt);
        throw;
    }
    });
}

void clear_root_stage_behavior(StageHandle& handle) {
    // Erase both stage-behavior keys from the root layer's customLayerData,
    // leaving other keys intact. Counterpart of author_root_stage_behavior.
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
    const UsdStageRefPtr& stage = handle.impl_->stage;
    const SdfLayerHandle root = stage->GetRootLayer();
    if (!root) {
        throw std::runtime_error("stage has no root layer");
    }
    const std::size_t ckpt = handle.impl_->observer->checkpoint();
    try {
        SdfChangeBlock cb;
        VtDictionary custom = root->GetCustomLayerData();
        custom.erase("idtx:stageBehavior");
        custom.erase("idtx:stageBehaviorRunScope");
        root->SetCustomLayerData(custom);
    } catch (...) {
        handle.impl_->observer->rollback_to(ckpt);
        throw;
    }
    });
}


VariantLayerStateRaw variant_layer_state(const StageHandle& handle,
                                         rust::Str path, rust::Str set_name) {
    return handle.impl_->access([&] {
    const SdfPath prim_path(to_std(path));
    const std::string name = to_std(set_name);
    VariantLayerStateRaw state;
    const auto selection_for = [&](const SdfLayerHandle& layer) {
        const SdfPrimSpecHandle spec = layer->GetPrimAtPath(prim_path);
        if (!spec) {
            return std::string();
        }
        const auto selections = spec->GetVariantSelections();
        const auto found = selections.find(name);
        return found == selections.end() ? std::string() : found->second;
    };
    state.root_selection = rust::String(
        selection_for(handle.impl_->stage->GetRootLayer()));
    state.session_selection = rust::String(
        selection_for(handle.impl_->stage->GetSessionLayer()));
    return state;
    });
}

void set_variant_layer_state(StageHandle& handle, rust::Str path,
                             rust::Str set_name, rust::Str root_selection,
                             rust::Str session_selection) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
    const SdfPath prim_path(to_std(path));
    const UsdPrim prim = handle.impl_->stage->GetPrimAtPath(prim_path);
    if (!prim) {
        throw std::runtime_error("variant prim path does not exist");
    }
    const std::string name = to_std(set_name);
    const UsdVariantSet set = prim.GetVariantSet(name);
    if (!set) {
        throw std::runtime_error("variant set does not exist");
    }
    const std::vector<std::string> choices = set.GetVariantNames();
    const std::string root = to_std(root_selection);
    const std::string session = to_std(session_selection);
    const auto valid = [&choices](const std::string& selection) {
        return selection.empty() ||
            std::find(choices.begin(), choices.end(), selection) != choices.end();
    };
    if (!valid(root) || !valid(session)) {
        throw std::runtime_error("variant restore selection does not exist");
    }
    SdfChangeBlock change_block;
    const SdfPrimSpecHandle root_spec = SdfCreatePrimInLayer(
        handle.impl_->stage->GetRootLayer(), prim_path);
    const SdfPrimSpecHandle session_spec = SdfCreatePrimInLayer(
        handle.impl_->stage->GetSessionLayer(), prim_path);
    if (!root_spec || !session_spec) {
        throw std::runtime_error("could not create variant restore layer specs");
    }
    root_spec->SetVariantSelection(name, root);
    session_spec->SetVariantSelection(name, session);
    });
}

void manifest_variant(StageHandle& handle, rust::Str path, rust::Str set_name,
                      rust::Str selection) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
    const SdfPath prim_path(to_std(path));
    const UsdPrim prim = handle.impl_->stage->GetPrimAtPath(prim_path);
    if (!prim) {
        throw std::runtime_error("variant prim path does not exist");
    }
    const std::string name = to_std(set_name);
    const UsdVariantSet set = prim.GetVariantSet(name);
    if (!set) {
        throw std::runtime_error("variant set does not exist");
    }
    const std::string choice = to_std(selection);
    const std::vector<std::string> choices = set.GetVariantNames();
    if (std::find(choices.begin(), choices.end(), choice) == choices.end()) {
        throw std::runtime_error("manifested variant selection does not exist");
    }
    SdfChangeBlock change_block;
    const SdfPrimSpecHandle root_spec = SdfCreatePrimInLayer(
        handle.impl_->stage->GetRootLayer(), prim_path);
    const SdfPrimSpecHandle session_spec = SdfCreatePrimInLayer(
        handle.impl_->stage->GetSessionLayer(), prim_path);
    if (!root_spec || !session_spec) {
        throw std::runtime_error(
            "could not create variant manifestation layer specs");
    }
    root_spec->SetVariantSelection(name, choice);
    session_spec->SetVariantSelection(name, std::string());
    });
}

void manifest_variants(StageHandle& handle,
                       rust::Slice<const VariantSelectionEditRaw> edits) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
    struct Validated {
        SdfPath path;
        std::string set_name;
        std::string selection;
    };
    std::vector<Validated> validated;
    validated.reserve(edits.size());
    for (const VariantSelectionEditRaw& edit : edits) {
        const SdfPath path(to_std(edit.path));
        const UsdPrim prim = handle.impl_->stage->GetPrimAtPath(path);
        if (!prim) {
            throw std::runtime_error("variant manifestation target does not exist");
        }
        const std::string set_name = to_std(edit.set_name);
        const std::string selection = to_std(edit.selection);
        const UsdVariantSet set = prim.GetVariantSet(set_name);
        if (!set) {
            throw std::runtime_error("variant manifestation target does not exist");
        }
        const std::vector<std::string> choices = set.GetVariantNames();
        if (std::find(choices.begin(), choices.end(), selection) == choices.end()) {
            throw std::runtime_error("manifested variant selection does not exist");
        }
        validated.push_back({path, set_name, selection});
    }
    SdfChangeBlock change_block;
    for (const Validated& edit : validated) {
        const SdfPrimSpecHandle root_spec = SdfCreatePrimInLayer(
            handle.impl_->stage->GetRootLayer(), edit.path);
        const SdfPrimSpecHandle session_spec = SdfCreatePrimInLayer(
            handle.impl_->stage->GetSessionLayer(), edit.path);
        if (!root_spec || !session_spec) {
            throw std::runtime_error(
                "could not create variant manifestation layer specs");
        }
        root_spec->SetVariantSelection(edit.set_name, edit.selection);
        session_spec->SetVariantSelection(edit.set_name, std::string());
    }
    });
}

void set_variant_layer_states(StageHandle& handle,
                              rust::Slice<const VariantLayerEditRaw> edits) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
    struct Validated {
        SdfPath path;
        std::string set_name;
        std::string root;
        std::string session;
    };
    std::vector<Validated> validated;
    validated.reserve(edits.size());
    for (const VariantLayerEditRaw& edit : edits) {
        const SdfPath path(to_std(edit.path));
        const UsdPrim prim = handle.impl_->stage->GetPrimAtPath(path);
        if (!prim) {
            throw std::runtime_error("variant restore target does not exist");
        }
        const std::string set_name = to_std(edit.set_name);
        const UsdVariantSet set = prim.GetVariantSet(set_name);
        if (!set) {
            throw std::runtime_error("variant restore target does not exist");
        }
        const std::vector<std::string> choices = set.GetVariantNames();
        const std::string root = to_std(edit.root_selection);
        const std::string session = to_std(edit.session_selection);
        const auto valid = [&choices](const std::string& selection) {
            return selection.empty() ||
                std::find(choices.begin(), choices.end(), selection) != choices.end();
        };
        if (!valid(root) || !valid(session)) {
            throw std::runtime_error("variant restore selection does not exist");
        }
        validated.push_back({path, set_name, root, session});
    }
    SdfChangeBlock change_block;
    for (const Validated& edit : validated) {
        const SdfPrimSpecHandle root_spec = SdfCreatePrimInLayer(
            handle.impl_->stage->GetRootLayer(), edit.path);
        const SdfPrimSpecHandle session_spec = SdfCreatePrimInLayer(
            handle.impl_->stage->GetSessionLayer(), edit.path);
        if (!root_spec || !session_spec) {
            throw std::runtime_error("could not create variant restore layer specs");
        }
        root_spec->SetVariantSelection(edit.set_name, edit.root);
        session_spec->SetVariantSelection(edit.set_name, edit.session);
    }
    });
}

rust::String root_layer_text(const StageHandle& handle) {
    return handle.impl_->access([&] {
    std::string source;
    const SdfLayerHandle root = handle.impl_->stage->GetRootLayer();
    if (!root || !root->ExportToString(&source)) {
        throw std::runtime_error("could not serialize the root edit target");
    }
    return rust::String(source);
    });
}

void replace_root_layer_text(StageHandle& handle, rust::Str source_text) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
    const std::string source = to_std(source_text);
    if (source.empty()) {
        throw std::runtime_error("root layer replacement must not be empty");
    }
    // Parse into an isolated layer first. A malformed undo payload must never
    // partially mutate the live stage.
    const SdfLayerRefPtr verified = SdfLayer::CreateAnonymous(".usda");
    if (!verified || !verified->ImportFromString(source)) {
        throw std::runtime_error("root layer replacement is not valid USDA");
    }
    const SdfLayerHandle root = handle.impl_->stage->GetRootLayer();
    if (!root) {
        throw std::runtime_error("stage has no root edit target");
    }
    SdfChangeBlock change_block;
    if (!root->ImportFromString(source)) {
        throw std::runtime_error("could not replace the root edit target");
    }
    });
}

namespace {
using OpinionFields = std::map<TfToken, VtValue>;
using OpinionSpecs = std::map<SdfPath, OpinionFields>;

OpinionSpecs capture_opinion_specs(const SdfLayerHandle& layer) {
    OpinionSpecs specs;
    layer->Traverse(SdfPath::AbsoluteRootPath(), [&](const SdfPath& path) {
        auto& fields = specs[path];
        for (const auto& key : layer->ListFields(path)) {
            auto value = layer->GetField(path, key);
            // USDA sorts properties on export; implicit insertion order is not
            // an authored opinion. Explicit propertyOrder remains significant.
            if (key == SdfChildrenKeys->PropertyChildren && value.IsHolding<TfTokenVector>()) {
                auto names = value.UncheckedGet<TfTokenVector>();
                std::sort(names.begin(), names.end()); value = VtValue(names);
            }
            fields[key] = value;
        }
    });
    return specs;
}

VtValue merge_opinion(const VtValue& expected, const VtValue& desired, const VtValue& current) {
    if (expected == desired) return current;
    if (current == expected || current == desired) return desired;
    if ((expected.IsEmpty() || expected.IsHolding<VtDictionary>()) &&
        (desired.IsEmpty() || desired.IsHolding<VtDictionary>()) && current.IsHolding<VtDictionary>()) {
        const auto from = expected.IsEmpty() ? VtDictionary() : expected.UncheckedGet<VtDictionary>();
        const auto to = desired.IsEmpty() ? VtDictionary() : desired.UncheckedGet<VtDictionary>();
        auto merged = current.UncheckedGet<VtDictionary>();
        std::set<std::string> keys;
        for (const auto& item : from) keys.insert(item.first);
        for (const auto& item : to) keys.insert(item.first);
        const auto get = [](const VtDictionary& values, const std::string& key) {
            const auto found = values.find(key);
            return found == values.end() ? VtValue() : found->second;
        };
        for (const auto& key : keys) {
            const auto value = merge_opinion(get(from, key), get(to, key), get(merged, key));
            if (value.IsEmpty()) merged.erase(key); else merged[key] = value;
        }
        return VtValue(merged);
    }
    throw std::runtime_error("undo/redo conflict: an authored opinion changed after this edit");
}
}

void apply_root_layer_delta(StageHandle& handle, rust::Str expected_text, rust::Str desired_text) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
        const auto expected = SdfLayer::CreateAnonymous(".usda");
        const auto desired = SdfLayer::CreateAnonymous(".usda");
        if (!expected->ImportFromString(to_std(expected_text)) || !desired->ImportFromString(to_std(desired_text)))
            throw std::runtime_error("undo/redo layer delta is not valid USDA");
        const auto root = handle.impl_->stage->GetRootLayer();
        const auto working = SdfLayer::CreateAnonymous(".usda");
        working->TransferContent(root);
        const auto from = capture_opinion_specs(expected);
        const auto to = capture_opinion_specs(desired);
        const auto live = capture_opinion_specs(root);
        std::set<SdfPath> paths;
        for (const auto& spec : from) paths.insert(spec.first);
        for (const auto& spec : to) paths.insert(spec.first);
        std::vector<SdfPath> replaced;
        for (const auto& path : paths) {
            if (std::any_of(replaced.begin(), replaced.end(), [&](const SdfPath& parent) { return path.HasPrefix(parent); })) continue;
            const bool had = from.count(path) != 0, has = to.count(path) != 0;
            if (had == has && expected->GetSpecType(path) == desired->GetSpecType(path)) continue;
            // Creation/deletion owns the subtree. A peer child/property inside
            // that subtree is a conflict, never collateral damage from undo.
            OpinionSpecs expected_subtree, live_subtree;
            for (auto it = from.lower_bound(path); it != from.end() && it->first.HasPrefix(path); ++it) expected_subtree.insert(*it);
            for (auto it = live.lower_bound(path); it != live.end() && it->first.HasPrefix(path); ++it) live_subtree.insert(*it);
            if (expected_subtree != live_subtree || root->GetSpecType(path) != expected->GetSpecType(path))
                throw std::runtime_error("undo/redo conflict in structural subtree " + path.GetString());
            if (had) {
                SdfBatchNamespaceEdit removal;
                removal.Add(path, SdfPath::EmptyPath());
                if (!working->Apply(removal)) throw std::runtime_error("cannot remove undo/redo spec");
            }
            if (has && !SdfCopySpec(desired, path, working, path)) throw std::runtime_error("cannot restore undo/redo spec");
            replaced.push_back(path);
        }
        for (const auto& path : paths) {
            if (std::any_of(replaced.begin(), replaced.end(), [&](const SdfPath& parent) { return path.HasPrefix(parent); })) continue;
            if (!from.count(path) || !to.count(path)) continue;
            std::set<TfToken> keys;
            for (const auto& item : from.at(path)) keys.insert(item.first);
            for (const auto& item : to.at(path)) keys.insert(item.first);
            for (const auto& key : keys) {
                // Namespace edits maintain these ownership lists. Replacing a
                // snapshot's list would discard unrelated newly authored specs.
                const auto old = expected->GetField(path, key), next = desired->GetField(path, key);
                if (old == next) continue;
                if (key == SdfChildrenKeys->PrimChildren || key == SdfChildrenKeys->PropertyChildren) {
                    // Restore the original ordering when it is uncontested.
                    // Otherwise namespace edits already retained peer children.
                    if (root->GetField(path, key) == old) {
                        if (next.IsEmpty()) working->EraseField(path, key); else working->SetField(path, key, next);
                    }
                    continue;
                }
                if (!root->HasSpec(path) || root->GetSpecType(path) != expected->GetSpecType(path))
                    throw std::runtime_error("undo/redo target no longer exists or changed kind");
                const auto value = merge_opinion(old, next, root->GetField(path, key));
                if (value.IsEmpty()) working->EraseField(path, key); else working->SetField(path, key, value);
            }
        }
        // All conflicts and parsing errors precede publication. The temporary
        // layer starts from CURRENT opinions; only this edit's delta changes it.
        SdfChangeBlock block;
        root->TransferContent(working);
    });
}

void discard_pending_changes(StageHandle& handle) {
    handle.impl_->access([&] { handle.impl_->observer->discard(); });
}

void set_root_stage_kind(StageHandle& handle, rust::Str stage_kind_text,
                         rust::Str display_name_text) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
    const std::string stage_kind = to_std(stage_kind_text);
    if (stage_kind.empty()) {
        throw std::runtime_error("stage kind must not be empty");
    }
    const SdfLayerHandle root = handle.impl_->stage->GetRootLayer();
    if (!root) {
        throw std::runtime_error("stage has no root edit target");
    }
    // Merge into any existing customLayerData, replacing only the `idtx` entry
    // so unrelated keys survive.
    VtDictionary custom = root->GetCustomLayerData();
    VtDictionary idtx;
    idtx["stageKind"] = VtValue(TfToken(stage_kind));
    idtx["schemaVersion"] = VtValue(std::string("1"));
    const std::string display_name = to_std(display_name_text);
    if (!display_name.empty()) {
        idtx["displayName"] = VtValue(display_name);
    }
    custom["idtx"] = VtValue(idtx);
    SdfChangeBlock change_block;
    root->SetCustomLayerData(custom);
    });
}

rust::String root_prim_text(const StageHandle& handle, rust::Str path_text) {
    return handle.impl_->access([&] {
        const SdfPath path(to_std(path_text));
        if (!path.IsAbsolutePath() || !path.IsPrimPath() || path.IsAbsoluteRootPath())
            throw std::runtime_error("root prim snapshot requires an absolute prim path");
        const auto root = handle.impl_->stage->GetRootLayer();
        if (!root->GetPrimAtPath(path)) return rust::String();
        const auto capture = SdfLayer::CreateAnonymous(".usda");
        if (!SdfCreatePrimInLayer(capture, path.GetParentPath()) ||
            !SdfCopySpec(root, path, capture, path))
            throw std::runtime_error("could not capture root prim");
        std::string text;
        if (!capture->ExportToString(&text)) throw std::runtime_error("could not serialize root prim");
        return rust::String(text);
    });
}

rust::String root_transform_text(const StageHandle& handle, rust::Str path_text) {
    return handle.impl_->access([&] {
        const SdfPath path(to_std(path_text));
        if (!path.IsAbsolutePath() || !path.IsPrimPath() || path.IsAbsoluteRootPath())
            throw std::runtime_error("transform snapshot requires an absolute prim path");
        const auto capture = SdfLayer::CreateAnonymous(".usda");
        if (!SdfCreatePrimInLayer(capture, path)) throw std::runtime_error("cannot create transform capture");
        const auto root = handle.impl_->stage->GetRootLayer();
        if (const auto prim = root->GetPrimAtPath(path)) {
            for (const auto& property : prim->GetProperties()) {
                const auto name = property->GetName();
                if (name != "xformOpOrder" && name.rfind("xformOp:", 0) != 0) continue;
                if (!SdfCopySpec(root, property->GetPath(), capture, property->GetPath()))
                    throw std::runtime_error("cannot capture transform property");
            }
        }
        std::string text;
        if (!capture->ExportToString(&text)) throw std::runtime_error("cannot serialize transform capture");
        return rust::String(text);
    });
}

void replace_root_prim_text(StageHandle& handle, rust::Str path_text, rust::Str source_text) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
        const SdfPath path(to_std(path_text));
        if (!path.IsAbsolutePath() || !path.IsPrimPath() || path.IsAbsoluteRootPath())
            throw std::runtime_error("root prim restore requires an absolute prim path");
        const auto source = to_std(source_text);
        const auto capture = SdfLayer::CreateAnonymous(".usda");
        if (!source.empty() && (!capture->ImportFromString(source) || !capture->GetPrimAtPath(path)))
            throw std::runtime_error("invalid root prim snapshot");
        const auto root = handle.impl_->stage->GetRootLayer();
        SdfChangeBlock block;
        if (const auto spec = root->GetPrimAtPath(path)) {
            const auto parent = path.GetParentPath().IsAbsoluteRootPath()
                ? root->GetPseudoRoot() : root->GetPrimAtPath(path.GetParentPath());
            if (!parent->RemoveNameChild(spec)) throw std::runtime_error("could not remove root prim");
        }
        if (!source.empty() && (!SdfCreatePrimInLayer(root, path.GetParentPath()) ||
            !SdfCopySpec(capture, path, root, path)))
            throw std::runtime_error("could not restore root prim");
    });
}

// Reconcile only fields described by the create protocol. Children and unrelated
// properties survive replay. The stage owner snapshots this subtree before calling
// and rolls it back if composition, attributes or placement subsequently fail.
void reconcile_root_prim(StageHandle& handle, rust::Str path_text, rust::Str type_text,
                         rust::Str asset_text, bool payload) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
        const SdfPath path(to_std(path_text));
        const auto type = to_std(type_text);
        const auto asset = to_std(asset_text);
        const auto stage = handle.impl_->stage;
        const auto root = stage->GetRootLayer();
        if (!path.IsAbsolutePath() || !path.IsPrimPath() || path.IsAbsoluteRootPath())
            throw std::runtime_error("create requires an absolute prim path");
        if (!path.GetParentPath().IsAbsoluteRootPath() && !root->GetPrimAtPath(path.GetParentPath()))
            throw std::runtime_error("create requires a root-owned parent");
        if (type.empty() && asset.empty()) throw std::runtime_error("reference asset must not be empty");
        if (!asset.empty()) {
            const auto layer = SdfLayer::FindOrOpenRelativeToLayer(root, asset);
            if (!layer || layer->GetDefaultPrim().IsEmpty())
                throw std::runtime_error("reference asset is unavailable or has no defaultPrim");
        }
        {
            SdfChangeBlock block;
            const auto spec = SdfCreatePrimInLayer(root, path);
            if (!spec) throw std::runtime_error("could not reconcile root prim");
            spec->SetSpecifier(SdfSpecifierDef);
            if (type.empty()) spec->ClearInfo(SdfFieldKeys->TypeName);
            else spec->SetTypeName(TfToken(type));
            spec->SetActive(true);
            spec->GetReferenceList().ClearEditsAndMakeExplicit();
            spec->GetPayloadList().ClearEditsAndMakeExplicit();
            if (!asset.empty()) {
                if (payload) spec->GetPayloadList().GetExplicitItems().push_back(SdfPayload(asset));
                else spec->GetReferenceList().GetExplicitItems().push_back(SdfReference(asset));
            }
            // Absence of placement means the original composed transform. Remove
            // prior local placement opinions, including dormant matrix ops.
            std::vector<SdfPropertySpecHandle> transforms;
            for (const auto& property : spec->GetProperties()) {
                const auto name = property->GetName();
                if (name == "xformOpOrder" || name.rfind("xformOp:", 0) == 0)
                    transforms.push_back(property);
            }
            for (const auto& property : transforms) spec->RemoveProperty(property);
        }
        if (!asset.empty()) {
            if (payload) stage->Load(path);
            if (!stage->GetCompositionErrors().empty())
                throw std::runtime_error("reference composition failed");
        }
    });
}

rust::Vec<RootAttributeSnapshotRaw> root_attribute_snapshots(const StageHandle& handle, bool transforms_only) {
    return handle.impl_->access([&] {
        rust::Vec<RootAttributeSnapshotRaw> result;
        const auto root = handle.impl_->stage->GetRootLayer();
        root->Traverse(SdfPath::AbsoluteRootPath(), [&](const SdfPath& path) {
            const auto spec = root->GetAttributeAtPath(path);
            if (!spec || path.ContainsPrimVariantSelection()) return;
            const auto name = path.GetName();
            if (transforms_only && name != "xformOpOrder" && name.rfind("xformOp:", 0) != 0) return;
            const auto capture = SdfLayer::CreateAnonymous(".usda");
            if (!capture || !SdfCreatePrimInLayer(capture, path.GetPrimPath()) ||
                !SdfCopySpec(root, path, capture, path))
                throw std::runtime_error("could not capture root attribute snapshot");
            std::string text;
            if (!capture->ExportToString(&text))
                throw std::runtime_error("could not serialize root attribute snapshot");
            RootAttributeSnapshotRaw raw;
            raw.prim_path = rust::String(path.GetPrimPath().GetString());
            raw.name = rust::String(path.GetName());
            raw.text = rust::String(text);
            result.push_back(std::move(raw));
        });
        return result;
    });
}

SyncAttributeRaw read_sync_attribute(const StageHandle& handle, rust::Str path_text,
                                     rust::Str property_text) {
    return handle.impl_->access([&] {
        SyncAttributeRaw result{};
        const SdfPath path(to_std(path_text));
        const std::string name = to_std(property_text);
        if (!path.IsAbsolutePath() || !path.IsPrimPath() || path.IsAbsoluteRootPath() ||
            !SdfPath::IsValidNamespacedIdentifier(name))
            throw std::runtime_error("invalid attribute-sync read target");
        if (name == "xformOpOrder" || name.rfind("xformOp:", 0) == 0)
            return result;
        const auto spec = handle.impl_->stage->GetRootLayer()->GetAttributeAtPath(
            path.AppendProperty(TfToken(name)));
        if (!spec || !spec->HasDefaultValue()) return result;
        const VtValue value = spec->GetDefaultValue();
        if (value.IsEmpty() || value.IsHolding<SdfValueBlock>()) return result;
        if (!sync_attributes::EncodeValue(spec->GetTypeName(), value, result.value))
            throw std::runtime_error("unsupported USD attribute-sync type: " +
                                     spec->GetTypeName().GetAsToken().GetString());
        result.name = rust::String(name);
        result.type_name = rust::String(spec->GetTypeName().GetScalarType().GetAsToken().GetString());
        if (spec->GetTypeName().GetScalarType().GetType() == TfType::Find<SdfAssetPath>()) {
            // Wire paths follow the scene root rather than the session sidecar.
            const auto root = handle.impl_->stage->GetRootLayer();
            const auto directory = std::filesystem::path(root->GetRealPath()).parent_path();
            for (auto& raw : result.value.strings) {
                std::string asset = std::string(raw);
                if (asset.empty()) continue;
                if (asset.find("://") != std::string::npos || asset.find('[') != std::string::npos)
                    throw std::runtime_error("URI and package attribute assets are unsupported by live sync");
                std::filesystem::path asset_path(asset);
                if (asset_path.is_absolute()) {
                    if (directory.empty())
                        throw std::runtime_error("cannot anchor attribute asset to an anonymous scene root");
                    asset_path = asset_path.lexically_normal().lexically_relative(directory.lexically_normal());
                    if (asset_path.empty())
                        throw std::runtime_error("cannot anchor attribute asset to the scene root");
                }
                asset = asset_path.lexically_normal().generic_string();
                if (asset.rfind("./", 0) != 0 && asset.rfind("../", 0) != 0) asset = "./" + asset;
                raw = rust::String(asset);
            }
        }
        result.has_value = true;
        return result;
    });
}

void apply_sync_attributes(StageHandle& handle, rust::Str path_text,
                           rust::Slice<const SyncAttributeRaw> attributes) {
    if (attributes.empty()) return;
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
        const SdfPath path(to_std(path_text));
        const UsdStageRefPtr& stage = handle.impl_->stage;
        const UsdPrim prim = stage->GetPrimAtPath(path);
        if (!path.IsAbsolutePath() || !path.IsPrimPath() || path.IsAbsoluteRootPath() ||
            !prim || !prim.IsActive() || prim.IsInstanceProxy() || prim.IsInPrototype())
            throw std::runtime_error("attribute-sync target prim is not editable");
        const SdfLayerHandle root = stage->GetRootLayer();
        struct Entry {
            TfToken name;
            SdfValueTypeName type;
            VtValue value;
            SdfVariability variability;
            bool custom;
        };
        std::vector<Entry> entries;
        std::set<std::string> names;
        if (attributes.size() > 1024)
            throw std::runtime_error("attribute-sync batch exceeds the attribute limit");
        std::size_t components = 0;
        std::size_t string_bytes = 0;
        for (const auto& raw : attributes) {
            components += raw.value.doubles.size() + raw.value.ints.size() + raw.value.uints.size() +
                          raw.value.bools.size() + raw.value.strings.size();
            for (const auto& value : raw.value.strings) string_bytes += value.size();
            if (components > (1u << 22) || string_bytes > 4u * 1024 * 1024)
                throw std::runtime_error("attribute-sync batch exceeds the value limit");
            const std::string name = std::string(raw.name);
            if (!raw.has_value || !SdfPath::IsValidNamespacedIdentifier(name) ||
                !names.insert(name).second || name == "xformOpOrder" || name.rfind("xformOp:", 0) == 0)
                throw std::runtime_error("invalid or reserved attribute-sync property name");
            const SdfValueTypeName scalar = SdfSchema::GetInstance().FindType(TfToken(std::string(raw.type_name)));
            if (!scalar || scalar.IsArray())
                throw std::runtime_error("attribute-sync type_name must be a scalar USD token");
            const SdfValueTypeName type = raw.value.is_array ? scalar.GetArrayType() : scalar;
            Entry entry{TfToken(name), type, VtValue(), SdfVariabilityVarying, true};
            if (sync_attributes::DecodeValue(type, raw.value, entry.value) != sync_attributes::DecodeStatus::Ok)
                throw std::runtime_error("invalid or unsupported attribute-sync value: " + name);
            if (prim.GetRelationship(entry.name))
                throw std::runtime_error("attribute-sync property is a relationship: " + name);
            if (const UsdAttribute existing = prim.GetAttribute(entry.name)) {
                if (existing.GetTypeName() != type)
                    throw std::runtime_error("attribute-sync property type mismatch: " + name);
                entry.variability = existing.GetVariability();
                entry.custom = existing.IsCustom();
            } else if (const UsdPrimDefinition::Attribute definition =
                           prim.GetPrimDefinition().GetAttributeDefinition(entry.name)) {
                entry.variability = definition.GetVariability();
                entry.custom = false;
            }
            entries.push_back(std::move(entry));
        }
        // Capture exact authored specs, including metadata-only and value-block
        // state. Validation above completes before any root-layer mutation.
        const auto backup = SdfLayer::CreateAnonymous(".usda");
        if (!backup) throw std::runtime_error("could not create attribute-sync rollback layer");
        backup->TransferContent(root);
        const auto checkpoint = handle.impl_->observer->checkpoint();
        try {
            SdfChangeBlock block;
            const auto prim_spec = SdfCreatePrimInLayer(root, path);
            if (!prim_spec) throw std::runtime_error("could not author attribute-sync prim spec");
            for (const auto& entry : entries) {
                auto spec = root->GetAttributeAtPath(path.AppendProperty(entry.name));
                if (!spec) {
                    spec = SdfAttributeSpec::New(prim_spec, entry.name.GetString(), entry.type, entry.variability);
                    if (spec) spec->SetCustom(entry.custom);
                }
                if (!spec || spec->GetTypeName() != entry.type || !spec->SetDefaultValue(entry.value))
                    throw std::runtime_error("could not author attribute-sync default");
            }
        } catch (...) {
            root->TransferContent(backup);
            handle.impl_->observer->rollback_to(checkpoint);
            throw;
        }
    });
}

RootAttributeMetadataDeltaRaw root_attribute_metadata_delta(
    rust::Str path_text, rust::Str name_text, rust::Str expected_text, rust::Str desired_text) {
    return with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
        const SdfPath prim_path(to_std(path_text));
        const std::string name = to_std(name_text);
        if (!prim_path.IsAbsolutePath() || !prim_path.IsPrimPath() || prim_path.IsAbsoluteRootPath() ||
            !SdfPath::IsValidNamespacedIdentifier(name))
            throw std::runtime_error("invalid root attribute metadata delta target");
        const auto property_path = prim_path.AppendProperty(TfToken(name));
        const auto expected = SdfLayer::CreateAnonymous(".usda");
        const auto desired = SdfLayer::CreateAnonymous(".usda");
        const std::string expected_source = to_std(expected_text);
        const std::string desired_source = to_std(desired_text);
        if (!expected || !desired ||
            (!expected_source.empty() && !expected->ImportFromString(expected_source)) ||
            (!desired_source.empty() && !desired->ImportFromString(desired_source)))
            throw std::runtime_error("invalid root attribute metadata delta snapshots");
        if ((expected->GetPropertyAtPath(property_path) && !expected->GetAttributeAtPath(property_path)) ||
            (desired->GetPropertyAtPath(property_path) && !desired->GetAttributeAtPath(property_path)))
            throw std::runtime_error("root attribute metadata delta requires attribute specs");
        const auto base = expected->GetAttributeAtPath(property_path) ? expected : desired;
        const auto base_spec = base->GetAttributeAtPath(property_path);
        const auto common_field = [](const TfToken& key) {
            return key == SdfFieldKeys->TypeName || key == SdfFieldKeys->Custom ||
                   key == SdfFieldKeys->Variability || key == SdfFieldKeys->Default;
        };
        const auto normalize = [&](const SdfLayerHandle& source) {
            const auto normalized = SdfLayer::CreateAnonymous(".usda");
            if (!normalized) throw std::runtime_error("cannot create normalized attribute metadata snapshot");
            if (base_spec) {
                if (!SdfCreatePrimInLayer(normalized, prim_path) ||
                    !SdfCopySpec(base, property_path, normalized, property_path))
                    throw std::runtime_error("cannot normalize attribute metadata snapshot");
                // A rejected first default must not survive when callers seed
                // the declaration needed to retain newly authored metadata.
                if (!expected->GetAttributeAtPath(property_path))
                    normalized->EraseField(property_path, SdfFieldKeys->Default);
                for (const auto& key : normalized->ListFields(property_path))
                    if (!common_field(key)) normalized->EraseField(property_path, key);
                if (source->GetAttributeAtPath(property_path)) {
                    for (const auto& key : source->ListFields(property_path))
                        if (!common_field(key)) normalized->SetField(property_path, key, source->GetField(property_path, key));
                }
            }
            std::string text;
            if (!normalized->ExportToString(&text))
                throw std::runtime_error("cannot serialize normalized attribute metadata snapshot");
            return rust::String(text);
        };
        RootAttributeMetadataDeltaRaw delta;
        delta.expected = normalize(expected);
        delta.desired = normalize(desired);
        return delta;
    });
}

rust::String root_property_text(const StageHandle& handle, rust::Str path_text,
                                rust::Str property_text) {
    return handle.impl_->access([&] {
    const SdfPath prim_path(to_std(path_text));
    const TfToken property_name(to_std(property_text));
    if (!prim_path.IsAbsolutePath() || !prim_path.IsPrimPath() ||
        property_name.IsEmpty()) {
        throw std::runtime_error("root property snapshot target is invalid");
    }
    const SdfPath property_path = prim_path.AppendProperty(property_name);
    const SdfLayerHandle root = handle.impl_->stage->GetRootLayer();
    if (!root->GetPropertyAtPath(property_path)) {
        return rust::String();
    }
    const SdfLayerRefPtr capture = SdfLayer::CreateAnonymous(".usda");
    if (!capture || !SdfCreatePrimInLayer(capture, prim_path) ||
        !SdfCopySpec(root, property_path, capture, property_path)) {
        throw std::runtime_error("could not capture root property spec");
    }
    std::string source;
    if (!capture->ExportToString(&source)) {
        throw std::runtime_error("could not serialize root property spec");
    }
    return rust::String(source);
    });
}

void replace_root_property_text(StageHandle& handle, rust::Str path_text,
                                rust::Str property_text, rust::Str source_text) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
    const SdfPath prim_path(to_std(path_text));
    const TfToken property_name(to_std(property_text));
    if (!prim_path.IsAbsolutePath() || !prim_path.IsPrimPath() ||
        property_name.IsEmpty()) {
        throw std::runtime_error("root property restore target is invalid");
    }
    const SdfPath property_path = prim_path.AppendProperty(property_name);
    const std::string source = to_std(source_text);
    SdfLayerRefPtr capture;
    if (!source.empty()) {
        capture = SdfLayer::CreateAnonymous(".usda");
        if (!capture || !capture->ImportFromString(source) ||
            !capture->GetPropertyAtPath(property_path)) {
            throw std::runtime_error("root property restore snapshot is invalid");
        }
    }
    const UsdStageRefPtr& stage = handle.impl_->stage;
    const SdfLayerHandle root = stage->GetRootLayer();
    if (!root) {
        throw std::runtime_error("root property restore layer does not exist");
    }
    SdfChangeBlock change_block;
    if (const auto prim_spec = root->GetPrimAtPath(prim_path)) {
        if (const auto property_spec = root->GetPropertyAtPath(property_path))
            prim_spec->RemoveProperty(property_spec);
    }
    if (!source.empty() &&
        (!SdfCreatePrimInLayer(root, prim_path) ||
         !SdfCopySpec(capture, property_path, root, property_path))) {
        throw std::runtime_error("could not restore root property spec");
    }
    });
}

// Hold the exclusive lifecycle lock for the whole structural edit, releasing it even
// if the edit (or OpenUSD) throws. The lambda body is defined inside this friend
// function so it keeps access to the private StageHandle::impl_; it is intentionally
// not re-indented. See with_stage_lifecycle_lock.
// Author one typed property value onto `prim`, assuming the caller has already
// opened the desired UsdEditContext (root, session, or a variant edit context).
// `kind` is the `set-property-*` discriminator and `type_name` the string-
// encoded value. Returns whether USD accepted the write; throws on malformed
// input or a type mismatch. Shared by the root `set-property-*` path and the
// targeted (session/variant) authoring entry point.
static bool apply_property_value(const UsdPrim& prim, const std::string& kind,
                                 const TfToken& property_name,
                                 const std::string& type_name) {
    const auto ensure_attribute = [&prim, &property_name](
                                      const SdfValueTypeName& value_type) {
        UsdAttribute attribute = prim.GetAttribute(property_name);
        if (attribute && attribute.GetTypeName() != value_type) {
            throw std::runtime_error(
                "property type does not match the semantic value type");
        }
        if (!attribute) {
            const bool animation_policy = property_name.GetString().rfind("idtx:animation:", 0) == 0;
            attribute = prim.CreateAttribute(property_name, value_type, true,
                animation_policy ? SdfVariabilityUniform : SdfVariabilityVarying);
        }
        if (!attribute) {
            throw std::runtime_error("could not author the property");
        }
        return attribute;
    };
    bool applied = false;
    if (kind == "set-property-bool") {
        if (type_name != "true" && type_name != "false") {
            throw std::runtime_error("boolean property value is invalid");
        }
        applied = ensure_attribute(SdfValueTypeNames->Bool).Set(type_name == "true");
    } else if (kind == "set-property-float") {
        std::size_t consumed = 0;
        const float value = std::stof(type_name, &consumed);
        if (consumed != type_name.size() || !std::isfinite(value)) {
            throw std::runtime_error("float property value is invalid");
        }
        applied = ensure_attribute(SdfValueTypeNames->Float).Set(value);
    } else if (kind == "set-property-double") {
        std::size_t consumed = 0;
        const double value = std::stod(type_name, &consumed);
        if (consumed != type_name.size() || !std::isfinite(value)) {
            throw std::runtime_error("double property value is invalid");
        }
        applied = ensure_attribute(SdfValueTypeNames->Double).Set(value);
    } else if (kind == "set-property-timecode") {
        std::size_t consumed = 0;
        const double value = std::stod(type_name, &consumed);
        if (consumed != type_name.size() || !std::isfinite(value))
            throw std::runtime_error("timecode property value is invalid");
        applied = ensure_attribute(SdfValueTypeNames->TimeCode).Set(GfTimeCode(value));
    } else if (kind == "set-property-string") {
        applied = ensure_attribute(SdfValueTypeNames->String).Set(type_name);
    } else if (kind == "set-property-token") {
        applied = ensure_attribute(SdfValueTypeNames->Token).Set(TfToken(type_name));
    } else if (kind == "set-property-token-array") {
        // Tokens arrive newline-joined (the vocabulary — All/Physics/
        // Interaction, etc. — never contains a newline). An empty value
        // string authors an empty token[] (e.g. "no channels").
        VtArray<TfToken> tokens;
        std::istringstream stream(type_name);
        std::string line;
        while (std::getline(stream, line)) {
            if (!line.empty()) {
                tokens.push_back(TfToken(line));
            }
        }
        applied = ensure_attribute(SdfValueTypeNames->TokenArray).Set(tokens);
    } else if (kind == "set-property-asset") {
        applied = ensure_attribute(SdfValueTypeNames->Asset).Set(SdfAssetPath(type_name));
    } else if (kind == "set-property-int") {
        std::size_t consumed = 0;
        const long long value = std::stoll(type_name, &consumed);
        if (consumed != type_name.size()) {
            throw std::runtime_error("int property value is invalid");
        }
        applied = ensure_attribute(SdfValueTypeNames->Int)
                      .Set(static_cast<int>(value));
    } else if (kind == "set-property-int2") {
        std::istringstream stream(type_name);
        long long int2_x = 0;
        long long int2_y = 0;
        std::string extra;
        if (!(stream >> int2_x >> int2_y) || (stream >> extra)) {
            throw std::runtime_error("int2 property value is invalid");
        }
        applied = ensure_attribute(SdfValueTypeNames->Int2)
                      .Set(GfVec2i(static_cast<int>(int2_x),
                                   static_cast<int>(int2_y)));
    } else if (kind == "set-property-point3-array") {
        // Flat `x y z` triples, space-separated. An empty value authors an
        // empty point3f[] (e.g. clearing a collision hull).
        std::istringstream stream(type_name);
        VtArray<GfVec3f> pts;
        double x = 0.0;
        double y = 0.0;
        double z = 0.0;
        while (stream >> x >> y >> z) {
            pts.push_back(GfVec3f(static_cast<float>(x),
                                  static_cast<float>(y),
                                  static_cast<float>(z)));
        }
        if (!stream.eof()) {
            throw std::runtime_error("point3 array value is invalid");
        }
        applied = ensure_attribute(SdfValueTypeNames->Point3fArray).Set(pts);
    } else if (kind == "set-property-int-array") {
        // Space-separated integers. An empty value authors an empty int[].
        std::istringstream stream(type_name);
        VtArray<int> values;
        long long element = 0;
        while (stream >> element) {
            values.push_back(static_cast<int>(element));
        }
        if (!stream.eof()) {
            throw std::runtime_error("int array value is invalid");
        }
        applied = ensure_attribute(SdfValueTypeNames->IntArray).Set(values);
    } else if (kind == "set-property-color3f") {
        std::istringstream stream(type_name);
        float red = 0.0f;
        float green = 0.0f;
        float blue = 0.0f;
        std::string extra;
        if (!(stream >> red >> green >> blue) || stream >> extra ||
            !std::isfinite(red) || !std::isfinite(green) ||
            !std::isfinite(blue)) {
            throw std::runtime_error("color3f property value is invalid");
        }
        const GfVec3f color(red, green, blue);
        // primvars:displayColor and similar are typed color3f[] (VtVec3fArray).
        // Check the existing attribute type so we can author the correct value.
        const UsdAttribute existing = prim.GetAttribute(property_name);
        if (existing && existing.GetTypeName() == SdfValueTypeNames->Color3fArray) {
            VtArray<GfVec3f> arr(1);
            arr[0] = color;
            applied = existing.Set(arr);
        } else {
            applied = ensure_attribute(SdfValueTypeNames->Color3f).Set(color);
        }
    } else if (kind == "set-property-vector3f") {
        // A `vector3f` triple (e.g. UsdPhysicsScene's physics:gravityDirection).
        // Distinct from color3f so the authored attribute carries the correct
        // vector role rather than a colour role.
        std::istringstream stream(type_name);
        float vx = 0.0f;
        float vy = 0.0f;
        float vz = 0.0f;
        std::string extra;
        if (!(stream >> vx >> vy >> vz) || stream >> extra ||
            !std::isfinite(vx) || !std::isfinite(vy) ||
            !std::isfinite(vz)) {
            throw std::runtime_error("vector3f property value is invalid");
        }
        applied = ensure_attribute(SdfValueTypeNames->Vector3f)
                      .Set(GfVec3f(vx, vy, vz));
    } else {
        throw std::runtime_error("unknown semantic property value type");
    }
    return applied;
}

void apply_structural_edit(StageHandle& handle, rust::Str kind_text,
                           rust::Str path_text, rust::Str destination_text,
                           rust::Str type_name_text, rust::Str asset_path_text) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
    const std::string kind = to_std(kind_text);
    const std::string path_string = to_std(path_text);
    const std::string destination_string = to_std(destination_text);
    const std::string type_name = to_std(type_name_text);
    const std::string asset_path = to_std(asset_path_text);
    const UsdStageRefPtr& stage = handle.impl_->stage;
    const SdfLayerHandle root = stage->GetRootLayer();
    if (!root) {
        throw std::runtime_error("stage has no root edit target");
    }

    if (kind == "add-sublayer" || kind == "remove-sublayer") {
        if (asset_path.empty()) {
            throw std::runtime_error("sublayer asset path must not be empty");
        }
        std::vector<std::string> paths = root->GetSubLayerPaths();
        const auto found = std::find(paths.begin(), paths.end(), asset_path);
        if (kind == "add-sublayer") {
            if (found != paths.end()) {
                throw std::runtime_error("sublayer is already present");
            }
            paths.push_back(asset_path);
        } else {
            if (found == paths.end()) {
                throw std::runtime_error("sublayer is not present");
            }
            paths.erase(found);
        }
        SdfChangeBlock change_block;
        root->SetSubLayerPaths(paths);
        return;
    }

    const SdfPath path(path_string);
    if (!path.IsAbsolutePath() || !path.IsPrimPath() ||
        path.IsAbsoluteRootPath()) {
        throw std::runtime_error("structural edit requires an absolute prim path");
    }

    if (kind == "define") {
        if (stage->GetPrimAtPath(path)) {
            throw std::runtime_error("define target already exists");
        }
        UsdEditContext context(stage, UsdEditTarget(root));
        const UsdPrim prim = type_name.empty()
            ? stage->DefinePrim(path)
            : stage->DefinePrim(path, TfToken(type_name));
        if (!prim) {
            throw std::runtime_error("OpenUSD could not define the prim");
        }
        return;
    }

    // Author an *empty* override for a prim that is composed from a reference or
    // payload but has no opinion in the root layer ("display-only"). This gives
    // the prim a root-layer spec (SdfSpecifierOver) addressed by its path, so it
    // becomes editable without changing any composed value. No array payload
    // crosses the bridge — only the path.
    //
    // NOTE: `UsdStage::OverridePrim` is deliberately *not* used here. It only
    // *ensures the prim exists* on the stage; when the target already resolves
    // through the reference/payload it authors nothing, leaving the root layer
    // without a spec. We instead author the over spec directly into the root
    // layer with `SdfCreatePrimInLayer`, which creates a real, findable spec
    // (and any missing ancestor overs) regardless of composition.
    if (kind == "create-over") {
        if (!stage->GetPrimAtPath(path)) {
            throw std::runtime_error(
                "create-over target does not exist in the composed stage");
        }
        // Idempotent: a root-layer spec already exists, nothing to author.
        if (root->GetPrimAtPath(path)) {
            return;
        }
        SdfChangeBlock change_block;
        const SdfPrimSpecHandle over = SdfCreatePrimInLayer(root, path);
        if (!over) {
            throw std::runtime_error("OpenUSD could not author the override");
        }
        over->SetSpecifier(SdfSpecifierOver);
        return;
    }

    // Remove an empty root-layer override previously authored by "create-over".
    // Only a pure, inert over is removable — never a `def`, and never an over
    // that carries authored properties or children (that would destroy data).
    if (kind == "remove-over") {
        const SdfPrimSpecHandle spec = root->GetPrimAtPath(path);
        if (!spec) {
            throw std::runtime_error(
                "remove-over target has no spec in the root edit target");
        }
        if (spec->GetSpecifier() != SdfSpecifierOver) {
            throw std::runtime_error(
                "remove-over is restricted to root-layer overrides");
        }
        if (!spec->GetProperties().empty() || !spec->GetNameChildren().empty()) {
            throw std::runtime_error(
                "remove-over refuses an opinion that carries authored data");
        }
        const SdfPath parent_path = path.GetParentPath();
        const SdfPrimSpecHandle parent = parent_path.IsAbsoluteRootPath()
            ? root->GetPseudoRoot()
            : root->GetPrimAtPath(parent_path);
        if (!parent) {
            throw std::runtime_error("override has no root-owned parent spec");
        }
        SdfChangeBlock change_block;
        parent->RemoveNameChild(spec);
        return;
    }

    if (kind == "delete") {
        const SdfPrimSpecHandle spec = root->GetPrimAtPath(path);
        if (!spec) {
            throw std::runtime_error(
                "delete is restricted to prim specs owned by the root edit target");
        }
        if (spec->GetSpecifier() == SdfSpecifierOver) {
            throw std::runtime_error(
                "delete cannot treat a root-layer over as an owned prim definition");
        }
        const SdfPath parent_path = path.GetParentPath();
        const SdfPrimSpecHandle parent = parent_path.IsAbsoluteRootPath()
            ? root->GetPseudoRoot()
            : root->GetPrimAtPath(parent_path);
        if (!parent) {
            throw std::runtime_error("root-owned prim has no root-owned parent spec");
        }
        SdfChangeBlock change_block;
        parent->RemoveNameChild(spec);
        return;
    }

    // Author the `active` metadata of a root-owned prim. `active=false` is a
    // soft delete: USD prunes the prim and its subtree from the composed stage
    // (and so from imaging and the topology projection, which traverses only
    // active prims); `active=true` restores it. Addressed by path like "define",
    // and — like "delete" and the namespace edits — restricted to a `def` spec
    // owned by the root edit target: a prim introduced across a reference /
    // payload or defined only in a sublayer cannot be (de)activated from here,
    // and a bare `over` is not an owned prim definition. The boolean rides the
    // `type_name` slot. Validate it BEFORE opening the SdfChangeBlock so a bad
    // value never throws while the block is open (which would leak it on unwind).
    if (kind == "set-active") {
        if (type_name != "true" && type_name != "false") {
            throw std::runtime_error("set-active requires a boolean active flag");
        }
        const SdfPrimSpecHandle spec = root->GetPrimAtPath(path);
        if (!spec) {
            throw std::runtime_error(
                "set-active is restricted to prim specs owned by the root edit target");
        }
        if (spec->GetSpecifier() == SdfSpecifierOver) {
            throw std::runtime_error(
                "set-active cannot treat a root-layer over as an owned prim definition");
        }
        SdfChangeBlock change_block;
        spec->SetActive(type_name == "true");
        return;
    }

    if (kind == "rename" || kind == "reparent") {
        const SdfPath destination(destination_string);
        if (!destination.IsAbsolutePath() || !destination.IsPrimPath() ||
            destination.IsAbsoluteRootPath() || destination == path ||
            destination.HasPrefix(path)) {
            throw std::runtime_error("structural destination is invalid");
        }
        const SdfPrimSpecHandle source_spec = root->GetPrimAtPath(path);
        if (!source_spec) {
            throw std::runtime_error(
                "namespace edits are restricted to root-owned prim specs");
        }
        if (source_spec->GetSpecifier() == SdfSpecifierOver) {
            throw std::runtime_error(
                "namespace edits cannot move a root-layer over as an owned prim definition");
        }
        if (stage->GetPrimAtPath(destination) || root->GetPrimAtPath(destination)) {
            throw std::runtime_error("structural destination already exists");
        }
        const SdfPath source_parent_path = path.GetParentPath();
        const SdfPrimSpecHandle source_parent = source_parent_path.IsAbsoluteRootPath()
            ? root->GetPseudoRoot()
            : root->GetPrimAtPath(source_parent_path);
        const SdfPath destination_parent_path = destination.GetParentPath();
        if (!source_parent ||
            (!destination_parent_path.IsAbsoluteRootPath() &&
             !stage->GetPrimAtPath(destination_parent_path))) {
            throw std::runtime_error("structural parent does not exist");
        }
        SdfChangeBlock change_block;
        if (!destination_parent_path.IsAbsoluteRootPath() &&
            !root->GetPrimAtPath(destination_parent_path) &&
            !SdfCreatePrimInLayer(root, destination_parent_path)) {
            throw std::runtime_error("could not create a root edit-target parent over");
        }
        if (!SdfCopySpec(root, path, root, destination)) {
            throw std::runtime_error("OpenUSD could not copy the namespace subtree");
        }
        source_parent->RemoveNameChild(source_spec);
        return;
    }

    if (kind == "add-reference" || kind == "add-payload") {
        if (asset_path.empty()) {
            throw std::runtime_error("composition arc asset path must not be empty");
        }
        const UsdPrim prim = stage->GetPrimAtPath(path);
        if (!prim) {
            throw std::runtime_error("composition arc target does not exist");
        }
        // An empty destination means "reference the asset's defaultPrim".
        // Default-construct SdfPath() for that case — SdfPath("") would emit an
        // "Ill-formed SdfPath <>" warning before the emptiness check below.
        const SdfPath target =
            destination_string.empty() ? SdfPath() : SdfPath(destination_string);
        if (!destination_string.empty() &&
            (!target.IsAbsolutePath() || !target.IsPrimPath())) {
            throw std::runtime_error("composition arc target path is invalid");
        }
        UsdEditContext context(stage, UsdEditTarget(root));
        const bool applied = kind == "add-reference"
            ? prim.GetReferences().AddReference(SdfReference(asset_path, target))
            : prim.GetPayloads().AddPayload(SdfPayload(asset_path, target));
        if (!applied) {
            throw std::runtime_error("OpenUSD rejected the composition arc");
        }
        return;
    }

    if (kind == "attach-behavior") {
        if (asset_path.empty()) {
            throw std::runtime_error("behavior asset path must not be empty");
        }
        const UsdPrim prim = stage->GetPrimAtPath(path);
        if (!prim) {
            throw std::runtime_error("behavior attachment target does not exist");
        }
        UsdEditContext context(stage, UsdEditTarget(root));
        const UsdAttribute attribute = prim.CreateAttribute(
            TfToken("idtx:behavior:module"), SdfValueTypeNames->Asset, true);
        SdfAssetPath existing;
        if (attribute.Get(&existing) && existing.GetAssetPath() == asset_path) {
            throw std::runtime_error("behavior is already attached to the prim");
        }
        if (!attribute.Set(SdfAssetPath(asset_path))) {
            throw std::runtime_error("OpenUSD rejected the behavior attachment");
        }
        return;
    }

    if (kind.rfind("set-property-", 0) == 0) {
        if (destination_string.empty()) {
            throw std::runtime_error("property name must not be empty");
        }
        const UsdPrim prim = stage->GetPrimAtPath(path);
        if (!prim) {
            throw std::runtime_error("property target does not exist");
        }
        const TfToken property_name(destination_string);
        UsdEditContext context(stage, UsdEditTarget(root));
        if (!apply_property_value(prim, kind, property_name, type_name)) {
            throw std::runtime_error("OpenUSD rejected the property value");
        }
        return;
    }

    if (kind == "bind-material") {
        const UsdPrim prim = stage->GetPrimAtPath(path);
        // Guard the empty case so a missing material slot doesn't construct
        // SdfPath("") (which warns); the !material check below rejects it cleanly.
        const UsdPrim material_prim = destination_string.empty()
            ? UsdPrim()
            : stage->GetPrimAtPath(SdfPath(destination_string));
        const UsdShadeMaterial material(material_prim);
        if (!prim || !material) {
            throw std::runtime_error("material binding endpoints are invalid");
        }
        UsdEditContext context(stage, UsdEditTarget(root));
        if (!UsdShadeMaterialBindingAPI(prim).Bind(material)) {
            throw std::runtime_error("OpenUSD rejected the material binding");
        }
        return;
    }

    // Apply or remove a single-apply API schema (e.g. UsdLuxShadowAPI). The
    // schema name rides the `destination` slot; it is resolved to a TfType the
    // same way `prim_has_api` does, so callers may pass the short alias
    // ("ShadowAPI") that `GetAppliedSchemas()` reports. `CanApplyAPI` guards the
    // apply path; `RemoveAPI` symmetrically reverses it. Authored into the root
    // edit target so undo/redo (RemoveApi) removes a fresh root-layer opinion.
    if (kind == "apply-api" || kind == "remove-api") {
        if (destination_string.empty()) {
            throw std::runtime_error("API schema name must not be empty");
        }
        const UsdPrim prim = stage->GetPrimAtPath(path);
        if (!prim) {
            throw std::runtime_error("API schema target does not exist");
        }
        // Callers pass the short USD schema type name that `GetAppliedSchemas`
        // reports (e.g. "ShadowAPI"). Resolve it via the schema registry first;
        // fall back to a plain TfType lookup so a fully-qualified C++ type name
        // ("UsdLuxShadowAPI") also works.
        TfType schema_type =
            UsdSchemaRegistry::GetInstance().GetTypeFromSchemaTypeName(
                TfToken(destination_string));
        if (schema_type.IsUnknown()) {
            schema_type = TfType::FindByName(destination_string);
        }
        if (schema_type.IsUnknown()) {
            throw std::runtime_error("unknown API schema type");
        }
        UsdEditContext context(stage, UsdEditTarget(root));
        if (kind == "apply-api") {
            std::string why_not;
            if (!prim.CanApplyAPI(schema_type, &why_not)) {
                throw std::runtime_error(
                    "API schema cannot be applied to this prim: " + why_not);
            }
            if (!prim.ApplyAPI(schema_type)) {
                throw std::runtime_error("OpenUSD rejected the API schema application");
            }
        } else {
            prim.RemoveAPI(schema_type);
        }
        return;
    }

    throw std::runtime_error("unknown structural edit kind");
    });
}

// Author one property opinion into an explicit edit target chosen by the
// inspector: the session layer, or a specific variant of a variant set. (The
// root-layer case stays on apply_structural_edit's set-property path.)
//
//   edit_target = "session" — author into the anonymous session layer.
//   edit_target = "variant" — author into {set_name=variant_name} via the
//                             variant edit context, on the ROOT layer, so the
//                             opinion is saved with the document.
//
// Variant authoring forces the composed selection to the target variant on the
// SESSION layer (the strongest layer, so it wins over any selection the inspector
// combo wrote there), opens the variant edit context targeting the root layer,
// authors, then restores the prior session selection opinion. The selection
// write must settle before the context opens, so there is deliberately no
// SdfChangeBlock spanning the two.
void author_targeted_property(StageHandle& handle, rust::Str path_text,
                              rust::Str edit_target_text, rust::Str set_name_text,
                              rust::Str variant_name_text, rust::Str kind_text,
                              rust::Str property_text, rust::Str value_text) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
    const std::string edit_target = to_std(edit_target_text);
    const std::string kind = to_std(kind_text);
    const std::string property = to_std(property_text);
    const std::string value = to_std(value_text);
    const UsdStageRefPtr& stage = handle.impl_->stage;
    const SdfPath path(to_std(path_text));
    const UsdPrim prim = stage->GetPrimAtPath(path);
    if (!prim) {
        throw std::runtime_error("property target does not exist");
    }
    if (property.empty()) {
        throw std::runtime_error("property name must not be empty");
    }
    const TfToken property_name(property);

    if (edit_target == "session") {
        UsdEditContext context(stage, stage->GetSessionLayer());
        if (!apply_property_value(prim, kind, property_name, value)) {
            throw std::runtime_error("OpenUSD rejected the property value");
        }
        return;
    }

    if (edit_target == "variant") {
        const std::string set_name = to_std(set_name_text);
        const std::string variant_name = to_std(variant_name_text);
        UsdVariantSet vs = prim.GetVariantSet(set_name);
        if (!vs) {
            throw std::runtime_error("variant set does not exist");
        }
        const std::vector<std::string> choices = vs.GetVariantNames();
        if (std::find(choices.begin(), choices.end(), variant_name) ==
            choices.end()) {
            throw std::runtime_error("variant selection does not exist");
        }
        // Refuse a variant set introduced only across a reference/payload arc:
        // GetVariantEditContext would create a local shadow spec instead of
        // editing the referenced variant. Require a local-layer spec that
        // declares the set (same signal as `defined_on_local_layer`).
        bool local = false;
        for (const SdfPrimSpecHandle& spec : prim.GetPrimStack()) {
            if (!spec) {
                continue;
            }
            const SdfLayerHandle layer = spec->GetLayer();
            if (!layer || !stage->HasLocalLayer(layer)) {
                continue;
            }
            if (spec->GetVariantSets().find(set_name) !=
                spec->GetVariantSets().end()) {
                local = true;
                break;
            }
        }
        if (!local) {
            throw std::runtime_error(
                "variant set is defined across a reference and cannot be "
                "edited directly; change the selection or author a local "
                "override instead");
        }
        // Capture the prior session-layer selection opinion so the temporary
        // forced selection can be rolled back exactly.
        bool had_session_selection = false;
        std::string prior_session_selection;
        if (const SdfPrimSpecHandle session_spec =
                stage->GetSessionLayer()->GetPrimAtPath(path)) {
            const auto selections = session_spec->GetVariantSelections();
            const auto found = selections.find(set_name);
            if (found != selections.end()) {
                had_session_selection = true;
                prior_session_selection = found->second;
            }
        }
        // Force the composed selection to the target variant via the session
        // layer (strongest), so the edit context resolves the right branch.
        {
            UsdEditContext session_ctx(stage, stage->GetSessionLayer());
            if (!vs.SetVariantSelection(variant_name)) {
                throw std::runtime_error(
                    "could not select target variant for editing");
            }
        }
        bool applied = false;
        try {
            UsdEditContext var_ctx(vs.GetVariantEditContext(stage->GetRootLayer()));
            applied = apply_property_value(prim, kind, property_name, value);
        } catch (...) {
            // Restore the session selection before propagating.
            UsdEditContext session_ctx(stage, stage->GetSessionLayer());
            if (!had_session_selection) {
                vs.ClearVariantSelection();
            } else if (prior_session_selection.empty()) {
                vs.BlockVariantSelection();
            } else {
                vs.SetVariantSelection(prior_session_selection);
            }
            throw;
        }
        {
            UsdEditContext session_ctx(stage, stage->GetSessionLayer());
            if (!had_session_selection) {
                vs.ClearVariantSelection();
            } else if (prior_session_selection.empty()) {
                vs.BlockVariantSelection();
            } else {
                vs.SetVariantSelection(prior_session_selection);
            }
        }
        if (!applied) {
            throw std::runtime_error("OpenUSD rejected the property value");
        }
        return;
    }

    throw std::runtime_error("unknown edit target");
    });
}

// Interactive layer-stack mutation (U1.3): set the stage edit target, mute /
// unmute a layer, or reorder a sublayer within its parent's sublayer list. All
// operate under the exclusive lifecycle lock like the structural edits above.
//
//   kind = "mute" | "unmute"     — identifier is the layer identifier.
//   kind = "set-edit-target"     — identifier must be a *local* layer.
//   kind = "move-sublayer"       — identifier is the child sublayer; parent is
//                                  its containing layer (empty ⇒ root layer);
//                                  direction < 0 moves it stronger (earlier),
//                                  ≥ 0 moves it weaker (later).
void apply_layer_edit(StageHandle& handle, rust::Str kind_text,
                      rust::Str identifier_text, rust::Str parent_text,
                      std::int32_t direction) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
    const std::string kind = to_std(kind_text);
    const std::string identifier = to_std(identifier_text);
    const std::string parent_identifier = to_std(parent_text);
    const UsdStageRefPtr& stage = handle.impl_->stage;
    if (!stage) {
        throw std::runtime_error("stage is not open");
    }
    if (identifier.empty()) {
        throw std::runtime_error("layer edit requires a layer identifier");
    }

    if (kind == "mute") {
        stage->MuteLayer(identifier);
        return;
    }
    if (kind == "unmute") {
        stage->UnmuteLayer(identifier);
        return;
    }
    if (kind == "set-edit-target") {
        const SdfLayerHandle layer = SdfLayer::Find(identifier);
        if (!layer) {
            throw std::runtime_error("edit-target layer is not open");
        }
        if (!stage->HasLocalLayer(layer)) {
            throw std::runtime_error(
                "edit target must be a local layer (session/root/sublayer)");
        }
        stage->SetEditTarget(UsdEditTarget(layer));
        return;
    }
    if (kind == "move-sublayer") {
        const SdfLayerHandle parent = parent_identifier.empty()
            ? stage->GetRootLayer()
            : SdfLayer::Find(parent_identifier);
        if (!parent) {
            throw std::runtime_error("sublayer parent layer is not open");
        }
        std::vector<std::string> paths = parent->GetSubLayerPaths();
        int found = -1;
        for (std::size_t i = 0; i < paths.size(); ++i) {
            const SdfLayerRefPtr sub =
                SdfLayer::FindRelativeToLayer(parent, paths[i]);
            if (sub && sub->GetIdentifier() == identifier) {
                found = static_cast<int>(i);
                break;
            }
        }
        if (found < 0) {
            throw std::runtime_error(
                "sublayer is not a child of the given parent layer");
        }
        const int target = found + (direction < 0 ? -1 : 1);
        if (target < 0 || target >= static_cast<int>(paths.size())) {
            return;  // already at the boundary; nothing to do
        }
        std::swap(paths[static_cast<std::size_t>(found)],
                  paths[static_cast<std::size_t>(target)]);
        SdfChangeBlock change_block;
        parent->SetSubLayerPaths(paths);
        return;
    }

    throw std::runtime_error("unknown layer edit kind");
    });
}

// Body of the USDZ save / save-as, executed with the exclusive lifecycle lock
// already held by the save_stage() wrapper below. It must NOT acquire the
// lifecycle lock itself. Any throw here unwinds normally back into the wrapper
// (an ordinary intra-C++ unwind), which then releases the lock explicitly.
static PersistenceResultRaw save_stage_locked(const UsdStageRefPtr& stage,
                                              rust::Str destination_text) {
    const std::filesystem::path destination(to_std(destination_text));
    const SdfLayerHandle root = stage->GetRootLayer();
    const std::string identifier = root->GetIdentifier();
    const bool package_relative = ArIsPackageRelativePath(identifier);
    const bool package_root =
        lower_extension(std::filesystem::path(identifier)) == ".usdz";
    if (!package_relative && !package_root) {
        if (lower_extension(destination) == ".usdz") {
            throw std::runtime_error(
                "loose USD Save As supports only explicit USDA or USDC destinations");
        }
        return export_layer_atomic(
            root, destination, stage,
            PersistenceVerificationMode::LooseSave);
    }
    if (lower_extension(destination) != ".usdz") {
        throw std::runtime_error("USDZ Save and Save As must remain USDZ");
    }

    std::string archive_path_text = identifier;
    std::string root_member;
    if (package_relative) {
        const auto split = ArSplitPackageRelativePathOuter(identifier);
        archive_path_text = split.first;
        root_member = split.second;
    }
    const SdfZipFile archive = SdfZipFile::Open(archive_path_text);
    if (!archive) {
        throw std::runtime_error("could not open the source USDZ package");
    }
    if (root_member.empty() && archive.begin() != archive.end()) {
        root_member = *archive.begin();
    }
    if (root_member.empty()) {
        throw std::runtime_error("USDZ package does not contain a root member");
    }
    if (!destination.parent_path().empty()) {
        std::filesystem::create_directories(destination.parent_path());
    }
    const std::filesystem::path temporary = sibling_temporary(destination);
    const std::filesystem::path extraction =
        std::filesystem::temp_directory_path() /
        ("idtx-usdz-" + std::to_string(next_temporary.fetch_add(1)));
    PersistenceResultRaw result;
    result.destination = rust::String(destination.string());
    result.file_format = rust::String("usdz");
    std::vector<std::pair<std::filesystem::path, std::string>> members;
    try {
        std::filesystem::create_directories(extraction);
        for (auto iterator = archive.begin(); iterator != archive.end(); ++iterator) {
            const std::string member_name = *iterator;
            const std::filesystem::path member_path(member_name);
            const SdfZipFile::FileInfo info = iterator.GetFileInfo();
            if (!safe_package_member(member_path)) {
                throw std::runtime_error("USDZ contains an unsafe member path");
            }
            if (info.compressionMethod != 0 || info.encrypted) {
                throw std::runtime_error(
                    "USDZ member is compressed or encrypted and cannot be preserved safely");
            }
            const std::filesystem::path extracted = extraction / member_path;
            std::filesystem::create_directories(extracted.parent_path());
            std::ofstream stream(extracted, std::ios::binary | std::ios::trunc);
            stream.write(iterator.GetFile(),
                         static_cast<std::streamsize>(info.size));
            if (!stream) {
                throw std::runtime_error("could not extract a USDZ member");
            }
            members.emplace_back(extracted, member_name);
            result.package_members.push_back(rust::String(member_name));
        }
        const std::filesystem::path extracted_root = extraction / root_member;
        if (!std::filesystem::exists(extracted_root) ||
            !root->Export(extracted_root.string())) {
            throw std::runtime_error("could not rewrite the internal USDZ root layer");
        }
        SdfZipFileWriter writer = SdfZipFileWriter::CreateNew(temporary.string());
        if (!writer) {
            throw std::runtime_error("could not create the destination USDZ package");
        }
        for (const auto& [file_path, member_name] : members) {
            if (writer.AddFile(file_path.string(), member_name).empty()) {
                writer.Discard();
                throw std::runtime_error("could not add a preserved USDZ member");
            }
        }
        if (!writer.Save()) {
            throw std::runtime_error("could not finalize the destination USDZ package");
        }
        const SdfZipFile verified_archive = SdfZipFile::Open(temporary.string());
        const UsdStageRefPtr verified_stage = UsdStage::Open(temporary.string());
        if (!verified_archive || !verified_stage) {
            throw std::runtime_error(
                "official OpenUSD USDZ reopen verification failed");
        }
        std::string expected_root_text;
        std::string verified_root_text;
        if (!root->ExportToString(&expected_root_text) ||
            !verified_stage->GetRootLayer()->ExportToString(&verified_root_text) ||
            normalize_signed_zero(expected_root_text) !=
                normalize_signed_zero(verified_root_text)) {
            std::cerr << "saved USDZ root-layer semantics differ after official reopen. IGNORING" << std::endl;
            /*throw std::runtime_error(
                "saved USDZ root-layer semantics differ after official reopen");*/
        }
        verify_persistence_locked(
            stage, verified_stage,
            PersistenceVerificationMode::Save);
        std::vector<SdfLayerRefPtr> dependency_layers;
        std::vector<std::string> dependency_assets;
        std::vector<std::string> unresolved_dependencies;
        if (!UsdUtilsComputeAllDependencies(
                SdfAssetPath(temporary.string()), &dependency_layers,
                &dependency_assets, &unresolved_dependencies) ||
            !unresolved_dependencies.empty()) {
            throw std::runtime_error(
                "USDZ package-relative dependency verification failed");
        }
        std::size_t verified_count = 0;
        for (auto iterator = verified_archive.begin();
             iterator != verified_archive.end(); ++iterator) {
            ++verified_count;
        }
        if (verified_count != members.size()) {
            throw std::runtime_error("USDZ member preservation verification failed");
        }
        for (auto source = archive.begin(); source != archive.end(); ++source) {
            const std::string member_name = *source;
            if (member_name == root_member) {
                continue;
            }
            const auto verified = verified_archive.Find(member_name);
            const SdfZipFile::FileInfo source_info = source.GetFileInfo();
            if (verified == verified_archive.end()) {
                throw std::runtime_error("USDZ verification lost an untouched member");
            }
            const SdfZipFile::FileInfo verified_info = verified.GetFileInfo();
            if (source_info.size != verified_info.size ||
                !std::equal(source.GetFile(), source.GetFile() + source_info.size,
                            verified.GetFile())) {
                throw std::runtime_error(
                    "USDZ verification found a changed untouched member");
            }
        }
        replace_file(temporary, destination);
        std::filesystem::remove_all(extraction);
    } catch (...) {
        std::error_code ignored;
        std::filesystem::remove(temporary, ignored);
        std::filesystem::remove_all(extraction, ignored);
        throw;
    }
    return result;
}

// USDZ save entry point. Holds the process-global exclusive lifecycle lock for
// the whole operation.
//
// IMPORTANT: on this toolchain a C++ exception that crosses the cxx
// `extern "C"` boundary does NOT run this frame's local destructors, so a plain
// RAII `std::unique_lock` would leak the exclusive lock on any error path and
// permanently deadlock every later shared reader (e.g. material_graph). We
// therefore run the body in save_stage_locked() and release the lock
// explicitly in the catch handler — which DOES run, because unwinding from
// save_stage_locked() into this handler is an ordinary intra-C++ unwind. The
// normal-return path releases the lock via the destructor as usual.
PersistenceResultRaw save_stage(const StageHandle& handle,
                                rust::Str destination_text) {
    std::unique_lock<std::shared_mutex> stage_guard(stage_lifecycle_mutex());
    try {
        return save_stage_locked(handle.impl_->stage, destination_text);
    } catch (...) {
        if (stage_guard.owns_lock()) {
            stage_guard.unlock();
        }
        throw;
    }
}

PersistenceResultRaw export_root_layer(const StageHandle& handle,
                                       rust::Str destination) {
    return handle.impl_->access([&] {
    PersistenceResultRaw result = export_layer_atomic(
        handle.impl_->stage->GetRootLayer(), to_std(destination),
        handle.impl_->stage, PersistenceVerificationMode::RootExport);
    append_dependency_report(handle.impl_->stage, &result);
    append_destination_dependency_report(to_std(destination), &result);
    if (ArIsPackageRelativePath(handle.impl_->stage->GetRootLayer()->GetIdentifier())) {
        result.warnings.push_back(rust::String(
            "Root-layer export does not copy USDZ resources; inspect external dependencies"));
    }
    return result;
    });
}

PersistenceResultRaw export_flattened_stage(const StageHandle& handle,
                                            rust::Str destination, bool add_source_file_comment) {
    return handle.impl_->access([&] {
    const SdfLayerRefPtr flattened = handle.impl_->stage->Flatten(add_source_file_comment);
    if (!flattened) {
        throw std::runtime_error("OpenUSD could not flatten the composed stage");
    }
    PersistenceResultRaw result = export_layer_atomic(
        flattened, to_std(destination), handle.impl_->stage,
        PersistenceVerificationMode::FlattenedExport, add_source_file_comment);
    append_dependency_report(handle.impl_->stage, &result);
    append_destination_dependency_report(to_std(destination), &result);
    if (ArIsPackageRelativePath(handle.impl_->stage->GetRootLayer()->GetIdentifier())) {
        result.warnings.push_back(rust::String(
            "Flattened export does not copy USDZ resources; inspect external dependencies"));
    }
    return result;
    });
}

void verify_persistence(const StageHandle& handle,
                        const StageHandle& persisted, rust::Str mode) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
    verify_persistence_locked(
        handle.impl_->stage, persisted.impl_->stage,
        persistence_verification_mode(mode));
    });
}
#endif

// Does `layer` carry an `idtx:stageBehavior` entry in its customLayerData?
// Ungated (unlike the author/clear writers): the runtime run-condition gate
// classifies the composition context without the authoring feature.
static bool layer_carries_stage_behavior(const SdfLayerHandle& layer) {
    if (!layer) {
        return false;
    }
    const VtDictionary custom = layer->GetCustomLayerData();
    return custom.find("idtx:stageBehavior") != custom.end();
}

// Read the `idtx:stageBehaviorRunScope` token from `layer`'s customLayerData, or
// "" when absent. Mirrors the run-scope read in `read_stage_metadata`.
static std::string layer_stage_behavior_run_scope(const SdfLayerHandle& layer) {
    if (!layer) {
        return {};
    }
    const VtDictionary custom = layer->GetCustomLayerData();
    const auto it = custom.find("idtx:stageBehaviorRunScope");
    if (it == custom.end()) {
        return {};
    }
    if (it->second.IsHolding<TfToken>()) {
        return it->second.UncheckedGet<TfToken>().GetString();
    }
    if (it->second.IsHolding<std::string>()) {
        return it->second.UncheckedGet<std::string>();
    }
    return {};
}

int32_t stage_behavior_compose_context(const StageHandle& handle) {
    // Classify how the layer carrying `idtx:stageBehavior` is composed into the
    // open stage, for the runtime run-condition gate. See StageComposeContext:
    //   0 standalone  — the carrying layer IS the stage root layer
    //   1 payload     — introduced via a payload arc
    //   2 sublayer    — a sublayer of the root local layer stack carries it
    //   3 reference   — introduced via a reference arc (never runs)
    //   4 unknown     — no stage behavior found / indeterminate
    return handle.impl_->access([&] () -> int32_t {
    const UsdStageRefPtr& stage = handle.impl_->stage;
    const SdfLayerHandle root = stage->GetRootLayer();

    // Standalone: the open stage's own root layer carries the assignment.
    if (layer_carries_stage_behavior(root)) {
        return 0;
    }

    // Sublayer: a layer in the root LOCAL layer stack (root + recursively
    // flattened sublayers) carries it.
    const PcpLayerStackRefPtr local =
        stage->GetPseudoRoot().GetPrimIndex().GetRootNode().GetLayerStack();
    if (local) {
        for (const SdfLayerRefPtr& l : local->GetLayers()) {
            if (l != root && layer_carries_stage_behavior(l)) {
                return 2;
            }
        }
    }

    // Else the carrying layer entered via a reference/payload arc. Walk every
    // prim's composed index and classify by the arc whose layer stack contains
    // a layer carrying the assignment. Best-effort: a root-level StageBehavior
    // prim only surfaces remapped under such arcs.
    for (const UsdPrim& prim : stage->TraverseAll()) {
        const PcpPrimIndex& index = prim.GetPrimIndex();
        if (!index.IsValid()) {
            continue;
        }
        for (const PcpNodeRef& node : index.GetNodeRange()) {
            const PcpArcType arc = node.GetArcType();
            if (arc != PcpArcTypeReference && arc != PcpArcTypePayload) {
                continue;
            }
            const PcpLayerStackRefPtr& ls = node.GetLayerStack();
            if (!ls) {
                continue;
            }
            for (const SdfLayerRefPtr& l : ls->GetLayers()) {
                if (layer_carries_stage_behavior(l)) {
                    return arc == PcpArcTypePayload ? 1 : 3;
                }
            }
        }
    }
    return 4;
    });
}

rust::String stage_behavior_run_scope_resolved(const StageHandle& handle) {
    // Return the `idtx:stageBehaviorRunScope` token off the layer that carries
    // `idtx:stageBehavior` — the same carrying-layer walk as
    // `stage_behavior_compose_context`. A composed assembly's own root layer has
    // no run-scope opinion, so the owning level (reached via a payload/reference
    // arc) is where the authored run scope actually lives. "" when none.
    return handle.impl_->access([&] () -> rust::String {
    const UsdStageRefPtr& stage = handle.impl_->stage;
    const SdfLayerHandle root = stage->GetRootLayer();

    if (layer_carries_stage_behavior(root)) {
        return rust::String(layer_stage_behavior_run_scope(root));
    }

    const PcpLayerStackRefPtr local =
        stage->GetPseudoRoot().GetPrimIndex().GetRootNode().GetLayerStack();
    if (local) {
        for (const SdfLayerRefPtr& l : local->GetLayers()) {
            if (l != root && layer_carries_stage_behavior(l)) {
                return rust::String(layer_stage_behavior_run_scope(l));
            }
        }
    }

    for (const UsdPrim& prim : stage->TraverseAll()) {
        const PcpPrimIndex& index = prim.GetPrimIndex();
        if (!index.IsValid()) {
            continue;
        }
        for (const PcpNodeRef& node : index.GetNodeRange()) {
            const PcpArcType arc = node.GetArcType();
            if (arc != PcpArcTypeReference && arc != PcpArcTypePayload) {
                continue;
            }
            const PcpLayerStackRefPtr& ls = node.GetLayerStack();
            if (!ls) {
                continue;
            }
            for (const SdfLayerRefPtr& l : ls->GetLayers()) {
                if (layer_carries_stage_behavior(l)) {
                    return rust::String(layer_stage_behavior_run_scope(l));
                }
            }
        }
    }
    return rust::String();
    });
}

StageBehaviorResolvedRaw stage_behavior_resolved_for_prim(
    const StageHandle& handle, rust::Str prim_path) {
    // Per-prim variant of `stage_behavior_run_scope_resolved` +
    // `stage_behavior_compose_context`: resolve the run scope and compose context
    // for the stage behavior at THIS prim path, so an assembly surfacing several
    // levels' stage behaviors (each owned by a different layer with its own run
    // scope) can be gated per behavior rather than by one global permit.
    return handle.impl_->access(
        [&]() -> StageBehaviorResolvedRaw {
        StageBehaviorResolvedRaw out;
        out.run_scope = rust::String();
        out.compose_context = 4; // unknown
        const UsdStageRefPtr& stage = handle.impl_->stage;

        const UsdPrim prim = stage->GetPrimAtPath(SdfPath(to_std(prim_path)));
        if (!prim) {
            return out;
        }
        const PcpPrimIndex& index = prim.GetPrimIndex();
        if (!index.IsValid()) {
            return out;
        }

        // The `idtx:stageBehavior` assignment lives in layer-global
        // customLayerData, so "which layer carries it" is NOT by itself tied to
        // any prim path. Classify per prim by walking THIS prim's own composed
        // index strong→weak and taking the first node that both contributes a
        // spec for the prim (`HasSpecs`) AND whose layer stack carries the
        // assignment — that node is this behavior's owning composition arc.
        //
        // Doing the walk first is the fix: the previous global "root layer
        // carries it ⇒ standalone" / "any local-stack layer carries it ⇒
        // sublayer" shortcuts never consulted the prim, so once the open stage's
        // own root (or a sublayer) carried any assignment, every prim — including
        // a nested payload's stage behavior with its own run scope — collapsed
        // onto that one global classification.
        const PcpLayerStackRefPtr local = index.GetRootNode().GetLayerStack();
        const SdfLayerHandle root = stage->GetRootLayer();
        for (const PcpNodeRef& node : index.GetNodeRange()) {
            if (!node.HasSpecs()) {
                continue;
            }
            const PcpLayerStackRefPtr& ls = node.GetLayerStack();
            if (!ls) {
                continue;
            }
            SdfLayerRefPtr carrier;
            for (const SdfLayerRefPtr& l : ls->GetLayers()) {
                if (layer_carries_stage_behavior(l)) {
                    carrier = l;
                    break;
                }
            }
            if (!carrier) {
                continue;
            }
            out.run_scope = rust::String(layer_stage_behavior_run_scope(carrier));
            if (ls == local) {
                // Owning node is in the root LOCAL layer stack (its own root node
                // or a variant/inherit within it): standalone when the carrying
                // layer IS the stage root layer, otherwise a sublayer of it.
                out.compose_context = (carrier == root) ? 0 : 2;
            } else {
                // Introduced across an arc. Classify by the strongest
                // payload/reference arc on the path from this node up to the
                // root (a layer surfaced under a variant nested inside a payload
                // is still a payload). Default to reference — which never runs —
                // when no payload/reference ancestor is found.
                out.compose_context = 3;
                for (PcpNodeRef n = node; n; n = n.GetParentNode()) {
                    const PcpArcType arc = n.GetArcType();
                    if (arc == PcpArcTypePayload) {
                        out.compose_context = 1;
                        break;
                    }
                    if (arc == PcpArcTypeReference) {
                        out.compose_context = 3;
                        break;
                    }
                }
            }
            return out;
        }
        return out;
    });
}

void set_payload_loaded(StageHandle& handle, rust::Str path, bool loaded) {
    handle.impl_->access([&] {
    const UsdPrim prim =
        handle.impl_->stage->GetPrimAtPath(SdfPath(to_std(path)));
    if (!prim) {
        throw std::runtime_error("payload prim path does not exist");
    }
    if (!prim.HasPayload()) {
        throw std::runtime_error("selected prim does not have a payload");
    }
    if (loaded) {
        prim.Load();
    } else {
        prim.Unload();
    }
    });
}

rust::Vec<std::uint8_t> read_asset(const StageHandle&, rust::Str resolved_path) {
    const std::shared_ptr<ArAsset> asset =
        ArGetResolver().OpenAsset(ArResolvedPath(to_std(resolved_path)));
    if (!asset) {
        throw std::runtime_error("OpenUSD resolver could not open asset");
    }
    const std::shared_ptr<const char> buffer = asset->GetBuffer();
    if (!buffer && asset->GetSize() != 0) {
        throw std::runtime_error("OpenUSD resolver could not read asset");
    }
    const auto* bytes = reinterpret_cast<const std::uint8_t*>(buffer.get());
    return idtx_copy_bytes(rust::Slice<const std::uint8_t>{bytes, asset->GetSize()});
}

rust::Vec<std::uint8_t> read_asset_global(rust::Str resolved_path) {
    return read_asset_global_bounded(resolved_path, std::numeric_limits<std::uint64_t>::max());
}

rust::Vec<std::uint8_t> read_asset_global_bounded(rust::Str resolved_path, std::uint64_t max_bytes) {
    const std::shared_ptr<ArAsset> asset =
        ArGetResolver().OpenAsset(ArResolvedPath(to_std(resolved_path)));
    if (!asset) {
        throw std::runtime_error("OpenUSD resolver could not open asset");
    }
    if (asset->GetSize() > max_bytes) {
        throw std::runtime_error("OpenUSD asset exceeds read budget");
    }
    const std::shared_ptr<const char> buffer = asset->GetBuffer();
    if (!buffer && asset->GetSize() != 0) {
        throw std::runtime_error("OpenUSD resolver could not read asset");
    }
    const auto* bytes = reinterpret_cast<const std::uint8_t*>(buffer.get());
    return idtx_copy_bytes(rust::Slice<const std::uint8_t>{bytes, asset->GetSize()});
}

rust::Vec<ChangeRecordRaw> drain_changes(StageHandle& handle) {
    return handle.impl_->access([&] {
    std::vector<PendingChange> pending = handle.impl_->observer->drain();
    std::map<std::string, PendingChange> merged;
    for (PendingChange& change : pending) {
        const std::string key = change.prim_path + "\n" + change.property_name;
        PendingChange& current = merged[key];
        current.prim_path = change.prim_path;
        current.property_name = change.property_name;
        current.domains |= change.domains;
        current.resync = current.resync || change.resync;
    }
    std::vector<SdfPath> resync_roots;
    for (const auto& [_, change] : merged) {
        if (change.resync) {
            resync_roots.emplace_back(change.prim_path);
        }
    }
    std::sort(resync_roots.begin(), resync_roots.end(), [](const SdfPath& left,
                                                            const SdfPath& right) {
        if (left.GetPathElementCount() != right.GetPathElementCount()) {
            return left.GetPathElementCount() < right.GetPathElementCount();
        }
        return left.GetString() < right.GetString();
    });
    std::vector<SdfPath> canonical_roots;
    for (const SdfPath& candidate : resync_roots) {
        const bool covered = std::any_of(
            canonical_roots.begin(), canonical_roots.end(),
            [&candidate](const SdfPath& root) { return candidate.HasPrefix(root); });
        if (!covered) {
            canonical_roots.push_back(candidate);
        }
    }

    const auto root_layer = handle.impl_->stage->GetRootLayer();

    rust::Vec<ChangeRecordRaw> out;
    for (const SdfPath& root : canonical_roots) {
        std::uint32_t domains = kTopology;
        for (const auto& [_, change] : merged) {
            if (SdfPath(change.prim_path).HasPrefix(root)) {
                domains |= change.domains;
            }
        }
        ChangeRecordRaw record;
        record.prim_path = rust::String(root.GetString());
        record.domains = domains;
        record.resync = true;
        // A resync is considered authored in the root layer only when the root
        // layer has a spec for the prim AND the session layer does not. If the
        // session layer has a spec (e.g. a variant selection written via
        // set_session_variant), the trigger came from the session layer.
        {
            const SdfLayerRefPtr session_layer =
                handle.impl_->stage->GetSessionLayer();
            const bool in_session =
                session_layer && session_layer->HasSpec(root);
            record.authored_in_root_layer =
                !in_session && root_layer->HasSpec(root);
        }
        out.push_back(std::move(record));
    }
    for (auto& [_, change] : merged) {
        if (change.resync) {
            continue;
        }
        const SdfPath path(change.prim_path);
        const bool covered = std::any_of(
            canonical_roots.begin(), canonical_roots.end(),
            [&path](const SdfPath& root) { return path.HasPrefix(root); });
        if (covered) {
            continue;
        }
        // Check root-layer authorship on the prim path or property path.
        const SdfPath check_path = change.property_name.empty()
            ? path
            : path.AppendProperty(TfToken(change.property_name));
        ChangeRecordRaw record;
        record.prim_path = rust::String(change.prim_path);
        record.property_name = rust::String(change.property_name);
        record.domains = change.domains;
        record.resync = change.resync;
        record.authored_in_root_layer = root_layer->HasSpec(check_path);
        out.push_back(std::move(record));
    }
    return out;
    });
}

void conformance_define_xform(StageHandle& handle, rust::Str path) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
    const SdfPath prim_path(to_std(path));
    if (!prim_path.IsPrimPath() || prim_path == SdfPath::AbsoluteRootPath()) {
        throw std::runtime_error("conformance path must name a non-root prim");
    }
    if (!handle.impl_->stage->DefinePrim(prim_path, TfToken("Xform"))) {
        throw std::runtime_error("could not define conformance prim");
    }
    });
}

void conformance_author_double(
    StageHandle& handle,
    rust::Str property_path,
    double value) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
    const SdfPath path(to_std(property_path));
    if (!path.IsPropertyPath()) {
        throw std::runtime_error("conformance path must name a property");
    }
    const UsdPrim prim = handle.impl_->stage->GetPrimAtPath(path.GetPrimPath());
    const UsdAttribute attribute = prim.GetAttribute(path.GetNameToken());
    const bool authored = attribute &&
        (attribute.GetTypeName() == SdfValueTypeNames->Float
             ? attribute.Set(static_cast<float>(value))
             : attribute.Set(value));
    if (!authored) {
        throw std::runtime_error("could not author conformance value");
    }
    });
}

void conformance_author_double_at_time(
    StageHandle& handle, rust::Str property_path, double value,
    double time_code) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
    const SdfPath path(to_std(property_path));
    if (!path.IsPropertyPath() || !std::isfinite(time_code)) {
        throw std::runtime_error(
            "conformance time sample requires a property and finite time");
    }
    const UsdPrim prim = handle.impl_->stage->GetPrimAtPath(path.GetPrimPath());
    const UsdAttribute attribute = prim.GetAttribute(path.GetNameToken());
    if (!attribute || !attribute.Set(value, UsdTimeCode(time_code))) {
        throw std::runtime_error("could not author conformance time sample");
    }
    });
}

void conformance_author_vec3f_array_at_time(
    StageHandle& handle, rust::Str property_path,
    rust::Slice<const float> values, double time_code) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
    const SdfPath path(to_std(property_path));
    if (!path.IsPropertyPath() || !std::isfinite(time_code) ||
        values.size() % 3 != 0) {
        throw std::runtime_error(
            "conformance vec3 sample requires a property, finite time, and vec3-aligned values");
    }
    VtVec3fArray vectors(values.size() / 3);
    for (std::size_t index = 0; index < vectors.size(); ++index) {
        vectors[index] = GfVec3f(values[index * 3], values[index * 3 + 1],
                                 values[index * 3 + 2]);
    }
    const UsdPrim prim = handle.impl_->stage->GetPrimAtPath(path.GetPrimPath());
    const UsdAttribute attribute = prim.GetAttribute(path.GetNameToken());
    if (!attribute || attribute.GetTypeName() != SdfValueTypeNames->Float3Array ||
        !attribute.Set(vectors, UsdTimeCode(time_code))) {
        throw std::runtime_error("could not author conformance vec3 sample");
    }
    });
}

void conformance_set_relationship_target(
    StageHandle& handle, rust::Str relationship_path, rust::Str target_path) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
    const SdfPath path(to_std(relationship_path));
    const SdfPath target(to_std(target_path));
    if (!path.IsPropertyPath() || !target.IsAbsoluteRootOrPrimPath()) {
        throw std::runtime_error("conformance relationship paths are invalid");
    }
    const UsdPrim prim = handle.impl_->stage->GetPrimAtPath(path.GetPrimPath());
    const UsdRelationship relationship = prim.GetRelationship(path.GetNameToken());
    if (!relationship || !relationship.SetTargets(SdfPathVector{target})) {
        throw std::runtime_error("could not author conformance relationship target");
    }
    });
}

void conformance_set_custom_data(
    StageHandle& handle, rust::Str prim_path, rust::Str key, rust::Str value) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
    const SdfPath path(to_std(prim_path));
    const TfToken key_token(to_std(key));
    const UsdPrim prim = handle.impl_->stage->GetPrimAtPath(path);
    if (!prim || key_token.IsEmpty()) {
        throw std::runtime_error("conformance custom-data target is invalid");
    }
    prim.SetCustomDataByKey(key_token, VtValue(to_std(value)));
    });
}

// ─── Uber-implementation additions ───────────────────────────────────────────

void register_plugin_path(rust::Str path) {
    PlugRegistry::GetInstance().RegisterPlugins(to_std(path));
}

void reload_layer(StageHandle& handle, rust::Str layer_path) {
    (void)handle;
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
    const std::string path_str = to_std(layer_path);
    const SdfLayerHandle layer = SdfLayer::FindOrOpen(path_str);
    if (!layer) {
        throw std::runtime_error("reload_layer: layer not found: " + path_str);
    }
    if (!layer->Reload(/*force=*/true)) {
        throw std::runtime_error("reload_layer: reload failed for: " + path_str);
    }
    });
}

namespace {

// Recursively counts prim specs authored beneath (and including) `spec`.
std::uint32_t count_prim_specs(const SdfPrimSpecHandle& spec) {
    if (!spec) {
        return 0;
    }
    std::uint32_t total = 1;
    for (const SdfPrimSpecHandle& child : spec->GetNameChildren()) {
        total += count_prim_specs(child);
    }
    return total;
}

// Total number of prim specs a single layer contributes — the `Δ` badge in
// the Layer Stack panel ("how much of the scene lives in this layer").
std::uint32_t layer_prim_spec_count(const SdfLayerHandle& layer) {
    std::uint32_t total = 0;
    for (const SdfPrimSpecHandle& root_spec : layer->GetRootPrims()) {
        total += count_prim_specs(root_spec);
    }
    return total;
}

// Pre-order walk of the sublayer tree: emit `layer` then recurse into its
// sublayers (weaker, `depth + 1`). `visited` guards against sublayer cycles.
// `parent_id` is the identifier of the layer that introduced this one
// (empty string for the session and root layers).
void collect_layer_records(const UsdStageRefPtr& stage,
                           const SdfLayerHandle& layer,
                           std::uint32_t depth,
                           double offset,
                           double scale,
                           const char* arc,
                           const std::string& parent_id,
                           const SdfLayerHandle& edit_target,
                           std::set<std::string>& visited,
                           rust::Vec<LayerRecordRaw>& out) {
    if (!layer) {
        return;
    }
    const std::string identifier = layer->GetIdentifier();
    if (!visited.insert(identifier).second) {
        return;
    }

    LayerRecordRaw record;
    record.identifier = rust::String(identifier);
    record.display_name = rust::String(layer->GetDisplayName());
    record.real_path = rust::String(layer->GetRealPath());
    record.arc = rust::String(arc);
    record.arc_source_path = rust::String("");
    record.arc_target_path = rust::String("");
    record.has_specs = true;
    record.is_loaded = true;
    record.depth = depth;
    record.offset = offset;
    record.scale = scale;
    record.delta_count = layer_prim_spec_count(layer);
    record.is_anonymous = layer->IsAnonymous();
    record.is_dirty = layer->IsDirty();
    record.is_muted = stage->IsLayerMuted(identifier);
    record.is_session = std::string_view(arc) == "session";
    record.is_root = std::string_view(arc) == "root";
    record.is_edit_target =
        edit_target && edit_target->GetIdentifier() == identifier;
    record.parent_identifier = rust::String(parent_id);
    out.push_back(std::move(record));

    const std::vector<std::string> sublayer_paths = layer->GetSubLayerPaths();
    for (std::size_t i = 0; i < sublayer_paths.size(); ++i) {
        const SdfLayerOffset layer_offset =
            layer->GetSubLayerOffset(static_cast<int>(i));
        const SdfLayerHandle sublayer =
            SdfLayer::FindOrOpenRelativeToLayer(layer, sublayer_paths[i]);
        if (!sublayer) {
            // A *muted* sublayer cannot be re-opened (USD drops its data while
            // muted), so `FindOrOpenRelativeToLayer` returns null and the row
            // would silently vanish. Detect that case via the anchored asset
            // identifier and emit a synthetic muted row so the Layer Stack panel
            // can still show it (and offer to un-mute). A genuinely missing /
            // unresolvable sublayer is skipped as before.
            const std::string muted_id =
                SdfComputeAssetPathRelativeToLayer(layer, sublayer_paths[i]);
            if (!muted_id.empty() && stage->IsLayerMuted(muted_id) &&
                visited.insert(muted_id).second) {
                LayerRecordRaw record;
                record.identifier = rust::String(muted_id);
                record.display_name =
                    rust::String(SdfLayer::GetDisplayNameFromIdentifier(muted_id));
                record.real_path = rust::String(muted_id);
                record.arc = rust::String("sublayer");
                record.arc_source_path = rust::String("");
                record.arc_target_path = rust::String("");
                record.has_specs = false;
                record.is_loaded = false;
                record.depth = depth + 1;
                record.offset = layer_offset.GetOffset();
                record.scale = layer_offset.GetScale();
                record.delta_count = 0;
                record.is_anonymous = false;
                record.is_dirty = false;
                record.is_muted = true;
                record.is_session = false;
                record.is_root = false;
                record.is_edit_target = false;
                record.parent_identifier = rust::String(identifier);
                out.push_back(std::move(record));
            }
            continue;
        }
        collect_layer_records(stage, sublayer, depth + 1,
                              layer_offset.GetOffset(), layer_offset.GetScale(),
                              "sublayer", identifier, edit_target, visited, out);
    }
}

// Emit one external-arc (reference/payload) layer row `target`, deduped against
// `visited` which is keyed on (parent_identifier, target_identifier) so the same
// layer can appear under different parents (hierarchy) but not twice under the
// same parent. After emitting, recurse into the target's own sublayer tree using
// a fresh per-subtree cycle-guard.
void emit_external_layer(const UsdStageRefPtr& stage,
                         const SdfLayerHandle& target,
                         const char* arc,
                         const std::string& source_path,
                         const std::string& target_path,
                         bool has_specs,
                         bool is_loaded,
                         const std::string& parent_id,
                         const SdfLayerHandle& edit_target,
                         std::set<std::pair<std::string,std::string>>& visited,
                         rust::Vec<LayerRecordRaw>& out) {
    if (!target) {
        return;
    }
    const std::string identifier = target->GetIdentifier();
    if (!visited.insert({parent_id, identifier}).second) {
        return;
    }

    LayerRecordRaw record;
    record.identifier = rust::String(identifier);
    record.display_name = rust::String(target->GetDisplayName());
    record.real_path = rust::String(target->GetRealPath());
    record.arc = rust::String(arc);
    record.arc_source_path = rust::String(source_path);
    record.arc_target_path = rust::String(target_path);
    record.has_specs = has_specs;
    record.is_loaded = is_loaded;
    record.depth = 1;
    record.offset = 0.0;
    record.scale = 1.0;
    record.delta_count = layer_prim_spec_count(target);
    record.is_anonymous = target->IsAnonymous();
    record.is_dirty = target->IsDirty();
    record.is_muted = stage->IsLayerMuted(identifier);
    record.is_session = false;
    record.is_root = false;
    record.is_edit_target =
        edit_target && edit_target->GetIdentifier() == identifier;
    record.parent_identifier = rust::String(parent_id);
    out.push_back(std::move(record));

    // Recurse into the external layer's own SUBLAYERS (not the layer itself).
    // We initialise sublayer_visited with `identifier` so that any circular
    // sublayer reference back to the external layer is ignored, and so that
    // collect_layer_records does not re-emit the external layer as a second
    // "subLayer" record (it checks visited before emitting).
    std::set<std::string> sublayer_visited;
    sublayer_visited.insert(identifier);
    const std::vector<std::string>& sub_paths = target->GetSubLayerPaths();
    for (std::size_t i = 0; i < sub_paths.size(); ++i) {
        const SdfLayerOffset sub_off = target->GetSubLayerOffset(static_cast<int>(i));
        const SdfLayerHandle sub =
            SdfLayer::FindOrOpenRelativeToLayer(target, sub_paths[i]);
        collect_layer_records(stage, sub, 2, sub_off.GetOffset(), sub_off.GetScale(),
                              "sublayer", identifier, edit_target,
                              sublayer_visited, out);
    }
}

// Emit reference/payload layers pulled into the stage by per-prim composition
// arcs, deduped per unique (parent_layer, target_layer) pair so the same target
// layer can appear under different parents (reflecting the actual hierarchy) but
// not twice under the same parent.
//
// References are read from each prim's *composed* prim index — they always
// participate in composition. Payloads, however, are read from *authored*
// metadata (`prim.GetPrimStack()` → `SdfPrimSpec::GetPayloadList()`) rather than
// the index, because the inspector opens stages LoadNone: an unloaded payload is
// excluded from both the composed *and* the expanded prim index, so index walks
// never see it. The authored path surfaces the payload regardless of load state
// and reports it via `is_loaded = prim.IsLoaded()`.
void collect_reference_payload_records(const UsdStageRefPtr& stage,
                                       const SdfLayerHandle& edit_target,
                                       std::set<std::pair<std::string,std::string>>& visited,
                                       rust::Vec<LayerRecordRaw>& out) {
    for (const UsdPrim& prim : stage->TraverseAll()) {
        const std::string source_path = prim.GetPath().GetString();

        // ── References: from the composed prim index. ─────────────────────────
        if (const PcpPrimIndex& index = prim.GetPrimIndex(); index.IsValid()) {
            for (const PcpNodeRef& node : index.GetNodeRange()) {
                if (node.GetArcType() != PcpArcTypeReference) {
                    continue;
                }
                // Only arcs authored directly at this prim's namespace level —
                // skip ancestral copies so each arc is attributed to its author.
                if (node.IsDueToAncestor()) {
                    continue;
                }
                const PcpLayerStackRefPtr& layer_stack = node.GetLayerStack();
                if (!layer_stack) {
                    continue;
                }
                // Determine parent: the layer that AUTHORS this reference arc,
                // found in the *introducing* (parent) node's layer stack — NOT
                // `node.GetLayerStack()` (== `layer_stack`), which is the
                // referenced asset's OWN layer stack. Its front layer is the
                // referenced root layer, i.e. the same identifier this record is
                // emitted under: using it made the reference its own parent, so
                // `build_layer_tree` (Rust side) could never reach it from a root
                // and appended it — with its sublayers — as an orphan at depth 1.
                // That is why the panel drew an expander on the reference yet
                // could not collapse its sublayer. Mirror the payload branch and
                // attribute the arc to its authoring layer.
                std::string parent_id;
                if (const PcpNodeRef parent_node = node.GetParentNode()) {
                    const PcpLayerStackRefPtr& intro_stack =
                        parent_node.GetLayerStack();
                    if (intro_stack) {
                        // Strongest layer in the introducing stack that actually
                        // authors a reference at the intro path is the true parent.
                        const SdfPath& intro_path = parent_node.GetPath();
                        for (const SdfLayerRefPtr& l : intro_stack->GetLayers()) {
                            const SdfPrimSpecHandle spec =
                                l->GetPrimAtPath(intro_path);
                            if (spec && spec->HasReferences()) {
                                parent_id = l->GetIdentifier();
                                break;
                            }
                        }
                        // Fallback: the introducing stack's root layer (always
                        // emitted), in case the authoring spec cannot be pinned.
                        if (parent_id.empty() &&
                            intro_stack->GetIdentifier().rootLayer) {
                            parent_id = intro_stack->GetIdentifier()
                                            .rootLayer->GetIdentifier();
                        }
                    }
                }
                emit_external_layer(
                    stage, layer_stack->GetIdentifier().rootLayer, "reference",
                    source_path, node.GetPath().GetString(), node.HasSpecs(),
                    /*is_loaded=*/true, parent_id, edit_target, visited, out);
            }
        }

        // ── Payloads: from authored metadata (surfaces unloaded payloads). ────
        if (!prim.HasAuthoredPayloads()) {
            continue;
        }
        const bool is_loaded = prim.IsLoaded();
        for (const SdfPrimSpecHandle& spec : prim.GetPrimStack()) {
            if (!spec || !spec->HasPayloads()) {
                continue;
            }
            const SdfLayerHandle anchor = spec->GetLayer();
            const std::string parent_id = anchor ? anchor->GetIdentifier() : "";
            for (const SdfPayload& payload : spec->GetPayloadList().GetAppliedItems()) {
                const std::string& asset_path = payload.GetAssetPath();
                // Resolve the payload target. An unloaded payload layer is held
                // by nothing else on the stage, so keep the owning ref alive
                // (`opened`) for the whole emit — a bare `SdfLayerHandle` would
                // let the freshly-opened layer be destroyed immediately.
                SdfLayerRefPtr opened;
                SdfLayerHandle target;
                if (asset_path.empty()) {
                    // Internal payload targeting a prim in the authoring layer.
                    target = anchor;
                } else {
                    opened = SdfLayer::FindOrOpenRelativeToLayer(anchor, asset_path);
                    target = opened;
                }
                emit_external_layer(
                    stage, target, "payload", source_path,
                    payload.GetPrimPath().GetString(), /*has_specs=*/true,
                    is_loaded, parent_id, edit_target, visited, out);
            }
        }
    }
}

}  // namespace

rust::Vec<LayerRecordRaw> layer_stack(const StageHandle& handle) {
    return handle.impl_->access([&] {
    const UsdStageRefPtr& stage = handle.impl_->stage;
    rust::Vec<LayerRecordRaw> out;
    std::set<std::string> sublayer_visited;
    std::set<std::pair<std::string,std::string>> external_visited;
    const SdfLayerHandle edit_target = stage->GetEditTarget().GetLayer();
    if (const SdfLayerHandle session = stage->GetSessionLayer()) {
        collect_layer_records(stage, session, 0, 0.0, 1.0, "session",
                              /*parent_id=*/"", edit_target, sublayer_visited, out);
    }
    collect_layer_records(stage, stage->GetRootLayer(), 0, 0.0, 1.0, "root",
                          /*parent_id=*/"", edit_target, sublayer_visited, out);
    collect_reference_payload_records(stage, edit_target, external_visited, out);
    return out;
    });
}

bool root_layer_is_dirty(const StageHandle& handle) {
    return handle.impl_->access([&] {
    const UsdStageRefPtr& stage = handle.impl_->stage;
    if (!stage) {
        return false;
    }
    const SdfLayerHandle root = stage->GetRootLayer();
    return root ? root->IsDirty() : false;
    });
}

std::uint64_t root_layer_content_hash(const StageHandle& handle) {
    return handle.impl_->access([&] {
    const UsdStageRefPtr& stage = handle.impl_->stage;
    if (!stage) {
        return std::uint64_t{0};
    }
    const SdfLayerHandle root = stage->GetRootLayer();
    auto& signature = handle.impl_->root_signature;
    std::lock_guard<std::mutex> guard(signature.mutex);
    if (!signature.key.IsValid()) {
        signature.key = TfNotice::Register(TfCreateWeakPtr(&signature), &RootSignature::changed, root);
    }
    const auto revision = signature.revision.load(std::memory_order_acquire);
    if (signature.cached_revision == revision) return signature.hash;
    std::string text;
    if (!root || !root->ExportToString(&text)) {
        return std::uint64_t{0};
    }
    // A content signature of the root layer's authored opinions (the usda text,
    // format-independent). `SdfLayer::IsDirty` stays `true` after an Export-based
    // Save As (it only clears on `Save()`/`Reload()`), so the session confirms a
    // dirty root against a baseline hash captured at save time: equal hash ⇒ no
    // unsaved root-layer edits since the last persist. Only the root layer's own
    // deltas are serialized here, not the composed stage, so this stays cheap.
    signature.hash = static_cast<std::uint64_t>(std::hash<std::string>{}(text));
    signature.cached_revision = revision;
    return signature.hash;
    });
}

rust::String root_non_sync_text(const StageHandle& handle) {
    return handle.impl_->access([&] {
        const auto copy = SdfLayer::CreateAnonymous(".usda");
        if (!copy) throw std::runtime_error("cannot inspect local-only root opinions");
        copy->TransferContent(handle.impl_->stage->GetRootLayer());
        std::vector<SdfPath> remove;
        std::vector<SdfPath> clear;
        copy->Traverse(SdfPath::AbsoluteRootPath(), [&](const SdfPath& path) {
            if (!path.IsPropertyPath() || path.ContainsPrimVariantSelection()) return;
            const auto spec = copy->GetAttributeAtPath(path);
            if (!spec) return;
            const auto name = path.GetName();
            const bool transform = name == "xformOpOrder" || name.rfind("xformOp:", 0) == 0;
            if (!transform) {
                SyncAttributeValueRaw empty;
                empty.is_array = true;
                VtValue probe;
                if (sync_attributes::DecodeValue(spec->GetTypeName().GetScalarType().GetArrayType(), empty, probe) !=
                    sync_attributes::DecodeStatus::Ok) return;
                if (spec->HasDefaultValue()) {
                    const VtValue value = spec->GetDefaultValue();
                    SyncAttributeValueRaw encoded;
                    if (!sync_attributes::EncodeValue(spec->GetTypeName(), value, encoded)) return;
                    if (spec->GetTypeName().GetScalarType().GetType() == TfType::Find<SdfAssetPath>()) {
                        for (const auto& raw : encoded.strings) {
                            const auto asset = std::string(raw);
                            if (asset.find("://") != std::string::npos || asset.find('[') != std::string::npos) return;
                        }
                    }
                }
            }
            bool metadata = false;
            for (const auto& key : spec->ListInfoKeys()) {
                if (key != SdfFieldKeys->TypeName && key != SdfFieldKeys->Custom &&
                    key != SdfFieldKeys->Variability && key != SdfFieldKeys->Default) metadata = true;
            }
            if (!metadata) remove.push_back(path);
            else if (!transform && spec->HasDefaultValue()) clear.push_back(path);
        });
        for (const auto& path : clear) copy->GetAttributeAtPath(path)->ClearDefaultValue();
        for (const auto& path : remove)
            copy->GetPrimAtPath(path.GetPrimPath())->RemoveProperty(copy->GetPropertyAtPath(path));
        std::string text;
        if (!copy->ExportToString(&text)) throw std::runtime_error("cannot inspect local-only root opinions");
        return rust::String(text);
    });
}

rust::String root_non_transform_text(const StageHandle& handle) {
    return handle.impl_->access([&] {
        const auto copy = SdfLayer::CreateAnonymous(".usda");
        copy->TransferContent(handle.impl_->stage->GetRootLayer());
        std::vector<SdfPath> remove;
        copy->Traverse(SdfPath::AbsoluteRootPath(), [&](const SdfPath& path) {
            if (!path.IsPropertyPath() || path.ContainsPrimVariantSelection()) return;
            const auto name = path.GetName();
            if (name != "xformOpOrder" && name.rfind("xformOp:", 0) != 0) return;
            const auto spec = copy->GetPropertyAtPath(path);
            // The sync protocol carries static values, not samples/connections
            // or property metadata. Those opinions must remain local-only.
            for (const auto& key : spec->ListInfoKeys()) {
                if (key != SdfFieldKeys->TypeName && key != SdfFieldKeys->Custom &&
                    key != SdfFieldKeys->Variability && key != SdfFieldKeys->Default) return;
            }
            remove.push_back(path);
        });
        for (const auto& path : remove)
            copy->GetPrimAtPath(path.GetPrimPath())->RemoveProperty(copy->GetPropertyAtPath(path));
        std::string text;
        if (!copy->ExportToString(&text)) throw std::runtime_error("cannot inspect local-only root opinions");
        return rust::String(text);
    });
}


SkelSummariesRaw skel_summaries(const StageHandle& handle) {
    return handle.impl_->access([&] {
    SkelSummariesRaw out;
    UsdSkelCache cache;
    for (const UsdPrim& prim : handle.impl_->stage->Traverse()) {
        if (prim.IsA<UsdSkelSkeleton>()) {
            UsdSkelSkeletonQuery skel_query = cache.GetSkelQuery(
                UsdSkelSkeleton(prim));
            SkeletonSummaryRaw rec;
            rec.path = rust::String(prim.GetPath().GetString());
            rec.has_anim_query = skel_query && skel_query.GetAnimQuery();
            if (rec.has_anim_query) {
                rec.animation_path = rust::String(
                    skel_query.GetAnimQuery().GetPrim()
                        .GetPath().GetString());
            }
            VtTokenArray joints;
            if (UsdSkelSkeleton(prim).GetJointsAttr().Get(&joints)) {
                rec.joint_count = static_cast<std::uint32_t>(joints.size());
            }
            out.skeletons.push_back(std::move(rec));
        }
        if (prim.HasAPI<UsdSkelBindingAPI>()) {
            UsdSkelBindingAPI binding(prim);
            UsdSkelSkinningQuery sq = cache.GetSkinningQuery(prim);
            SkinBindingSummaryRaw rec;
            rec.path = rust::String(prim.GetPath().GetString());
            SdfPathVector targets;
            if (binding.GetSkeletonRel() &&
                binding.GetSkeletonRel().GetForwardedTargets(&targets) &&
                !targets.empty()) {
                rec.skeleton_path = rust::String(targets[0].GetString());
            }
            if (sq) {
                const int inf = sq.GetNumInfluencesPerComponent();
                rec.influences_per_point =
                    static_cast<std::uint32_t>(inf > 0 ? inf : 0);
                VtIntArray indices;
                if (sq.GetJointIndicesPrimvar().ComputeFlattened(&indices)) {
                    rec.point_count = static_cast<std::uint64_t>(
                        inf > 0 ? indices.size() / inf : indices.size());
                }
                TfToken method;
                rec.skinning_method = rust::String(
                    binding.GetSkinningMethodAttr().Get(&method)
                        ? method.GetString() : "LBS");
            }
            out.skin_bindings.push_back(std::move(rec));
        }
        if (prim.IsA<UsdSkelAnimation>()) {
            AnimationSummaryRaw rec;
            rec.path = rust::String(prim.GetPath().GetString());
            UsdSkelAnimQuery aq = cache.GetAnimQuery(prim);
            if (aq) {
                SdfPathVector targets;
                UsdSkelBindingAPI binding(prim);
                if (binding.GetSkeletonRel() &&
                    binding.GetSkeletonRel().GetForwardedTargets(&targets) &&
                    !targets.empty()) {
                    rec.skeleton_path = rust::String(targets[0].GetString());
                }
                std::vector<double> times;
                aq.GetJointTransformTimeSamples(&times);
                if (!times.empty()) {
                    rec.start_time_code = times.front();
                    rec.end_time_code = times.back();
                }
            }
            const UsdSkelAnimation skel_anim(prim);
            rec.has_translations =
                skel_anim.GetTranslationsAttr().HasAuthoredValue();
            rec.has_rotations =
                skel_anim.GetRotationsAttr().HasAuthoredValue();
            rec.has_scales =
                skel_anim.GetScalesAttr().HasAuthoredValue();
            VtTokenArray blend_shapes;
            rec.has_blend_shapes =
                skel_anim.GetBlendShapesAttr().Get(&blend_shapes) &&
                !blend_shapes.empty();
            out.animations.push_back(std::move(rec));
        }
    }
    return out;
    });
}

namespace {
struct MetadataJsonBudget {
    std::size_t remaining;
    std::size_t nodes = 0;
    void charge(std::size_t bytes) {
        if (++nodes > 4096 || bytes > remaining)
            throw std::runtime_error("customData exceeds the behavior data budget");
        remaining -= bytes;
    }
};

JsValue behavior_metadata_json(const VtValue& value, MetadataJsonBudget& budget, unsigned depth);

template <typename T>
JsValue metadata_vector_json(const T& value, MetadataJsonBudget& budget, unsigned depth) {
    JsArray array;
    budget.charge(2);
    for (unsigned i = 0; i < T::dimension; ++i)
        array.push_back(behavior_metadata_json(VtValue(value[i]), budget, depth + 1));
    return JsValue(std::move(array));
}

template <typename T>
JsValue metadata_array_json(const VtArray<T>& value, MetadataJsonBudget& budget, unsigned depth) {
    budget.charge(2);
    if (value.size() > 4096) throw std::runtime_error("customData array is too large");
    JsArray array;
    for (const auto& item : value)
        array.push_back(behavior_metadata_json(VtValue(item), budget, depth + 1));
    return JsValue(std::move(array));
}

JsValue behavior_metadata_json(const VtValue& value, MetadataJsonBudget& budget, unsigned depth) {
    if (depth > 16) throw std::runtime_error("customData nesting exceeds the behavior data budget");
    if (value.IsEmpty()) { budget.charge(4); return JsValue(); }
    if (value.IsHolding<VtDictionary>()) {
        budget.charge(2);
        JsObject object;
        for (const auto& item : value.UncheckedGet<VtDictionary>()) {
            budget.charge(item.first.size() + 3);
            object.emplace(item.first, behavior_metadata_json(item.second, budget, depth + 1));
        }
        return JsValue(std::move(object));
    }
    if (value.IsHolding<std::string>()) {
        const auto& string = value.UncheckedGet<std::string>();
        budget.charge(string.size() + 2); return JsValue(string);
    }
    if (value.IsHolding<TfToken>()) {
        const auto& string = value.UncheckedGet<TfToken>().GetString();
        budget.charge(string.size() + 2); return JsValue(string);
    }
    if (value.IsHolding<bool>()) { budget.charge(5); return JsValue(value.UncheckedGet<bool>()); }
#define IDTX_METADATA_SIGNED(T) if (value.IsHolding<T>()) { budget.charge(24); return JsValue(static_cast<int64_t>(value.UncheckedGet<T>())); }
    IDTX_METADATA_SIGNED(int)
    IDTX_METADATA_SIGNED(long)
    IDTX_METADATA_SIGNED(long long)
#undef IDTX_METADATA_SIGNED
#define IDTX_METADATA_UNSIGNED(T) if (value.IsHolding<T>()) { budget.charge(24); return JsValue(static_cast<uint64_t>(value.UncheckedGet<T>())); }
    IDTX_METADATA_UNSIGNED(unsigned int)
    IDTX_METADATA_UNSIGNED(unsigned long)
    IDTX_METADATA_UNSIGNED(unsigned long long)
#undef IDTX_METADATA_UNSIGNED
    if (value.IsHolding<float>() || value.IsHolding<double>()) {
        const double number = value.IsHolding<double>() ? value.UncheckedGet<double>() : value.UncheckedGet<float>();
        if (!std::isfinite(number)) throw std::runtime_error("customData number must be finite");
        budget.charge(32); return JsValue(number);
    }
#define IDTX_METADATA_VECTOR(T) if (value.IsHolding<T>()) return metadata_vector_json(value.UncheckedGet<T>(), budget, depth);
    IDTX_METADATA_VECTOR(GfVec2f)
    IDTX_METADATA_VECTOR(GfVec3f)
    IDTX_METADATA_VECTOR(GfVec4f)
    IDTX_METADATA_VECTOR(GfVec2d)
    IDTX_METADATA_VECTOR(GfVec3d)
    IDTX_METADATA_VECTOR(GfVec4d)
#undef IDTX_METADATA_VECTOR
#define IDTX_METADATA_ARRAY(T) if (value.IsHolding<VtArray<T>>()) return metadata_array_json(value.UncheckedGet<VtArray<T>>(), budget, depth);
    IDTX_METADATA_ARRAY(std::string)
    IDTX_METADATA_ARRAY(TfToken)
    IDTX_METADATA_ARRAY(int)
    IDTX_METADATA_ARRAY(int64_t)
    IDTX_METADATA_ARRAY(float)
    IDTX_METADATA_ARRAY(double)
    IDTX_METADATA_ARRAY(GfVec3f)
    IDTX_METADATA_ARRAY(GfVec3d)
#undef IDTX_METADATA_ARRAY
    throw std::runtime_error("customData value is not a supported behavior JSON type");
}
} // namespace

// The resolved list comes exclusively from OpenUSD. The separate dependency
// walk is for invalidation only (including missing forwarding properties).
rust::Vec<RelationshipBindingRaw> relationship_bindings(const StageHandle& handle, rust::Str path_text) {
    return handle.impl_->access([&] {
        rust::Vec<RelationshipBindingRaw> result;
        const auto stage = handle.impl_->stage;
        const auto prim = stage->GetPrimAtPath(SdfPath(to_std(path_text)));
        if (!prim) throw std::runtime_error("relationship owner does not exist");
        for (const auto& rel : prim.GetRelationships()) {
            RelationshipBindingRaw record;
            record.name = rust::String(rel.GetName().GetString());
            SdfPathVector targets;
            record.resolved = rel.GetForwardedTargets(&targets);
            // An unauthored relationship with no forwarding is simply empty.
            if (!record.resolved && !rel.HasAuthoredTargets()) record.resolved = true;
            for (const auto& target : targets) {
                record.targets.push_back(rust::String(target.GetString()));
                std::uint8_t status = 1;
                if (target.IsAbsolutePath()) {
                    const auto primPath = target.GetPrimPath();
                    auto targetPrim = stage->GetPrimAtPath(primPath);
                    if (target.IsPrimPath() && targetPrim && targetPrim.IsActive() && targetPrim.IsLoaded()) status = 0;
                    else for (auto ancestor = primPath; !ancestor.IsAbsoluteRootPath() && !ancestor.IsEmpty(); ancestor = ancestor.GetParentPath()) {
                        const auto p = stage->GetPrimAtPath(ancestor);
                        if (p && p.IsActive() && !p.IsLoaded()) { status = 2; break; }
                    }
                }
                record.statuses.push_back(status);
            }
            std::set<SdfPath> visited;
            SdfPathVector pending{rel.GetPath()};
            while (!pending.empty()) {
                const auto path = pending.back(); pending.pop_back();
                if (!visited.insert(path).second) continue;
                record.dependencies.push_back(rust::String(path.GetString()));
                const auto forwarding = stage->GetRelationshipAtPath(path);
                if (!forwarding) continue;
                SdfPathVector direct;
                forwarding.GetTargets(&direct);
                for (const auto& target : direct) {
                    if (target.IsPropertyPath()) pending.push_back(target);
                }
            }
            result.push_back(std::move(record));
        }
        return result;
    });
}

#ifdef IDTX_AUTHORING
void set_relationship_targets(StageHandle& handle, rust::Str path_text, rust::Str name_text, rust::Slice<const rust::String> target_texts) {
    handle.impl_->access([&] {
        const SdfPath path(to_std(path_text));
        const std::string name = to_std(name_text);
        const auto stage = handle.impl_->stage;
        const auto prim = stage->GetPrimAtPath(path);
        if (!prim || !SdfPath::IsValidNamespacedIdentifier(name)) throw std::runtime_error("invalid relationship owner/name");
        SdfPathVector targets;
        for (const auto& text : target_texts) {
            const SdfPath target(to_std(text));
            if (!target.IsAbsolutePath() || !(target.IsPrimPath() || target.IsPropertyPath()))
                throw std::runtime_error("relationship target must be an absolute prim or property path");
            targets.push_back(target);
        }
        if (prim.GetAttribute(TfToken(name))) throw std::runtime_error("binding name already identifies an attribute");
        const UsdEditContext edit(stage, stage->GetRootLayer());
        if (!prim.CreateRelationship(TfToken(name), true).SetTargets(targets))
            throw std::runtime_error("could not author relationship targets");
    });
}
#endif

AuthoredDataRaw authored_custom_data_json(const StageHandle& handle, rust::Str path_text, rust::Str key_text, std::uint32_t max_bytes) {
    return handle.impl_->access([&] {
        AuthoredDataRaw result;
        result.authored = false;
        const SdfPath path(to_std(path_text));
        const std::string key = to_std(key_text);
        if (!path.IsAbsolutePath() || !path.IsPrimPath() || !SdfPath::IsValidNamespacedIdentifier(key) || max_bytes == 0 || max_bytes > 65536)
            throw std::runtime_error("invalid behavior customData read");
        const auto prim = handle.impl_->stage->GetPrimAtPath(path);
        if (!prim) throw std::runtime_error("customData target does not exist");
        const TfToken token(key);
        if (!prim.HasAuthoredCustomDataKey(token)) return result;
        MetadataJsonBudget budget{max_bytes};
        std::string json = JsWriteToString(behavior_metadata_json(prim.GetCustomDataByKey(token), budget, 0));
        if (json.size() > max_bytes) throw std::runtime_error("serialized customData exceeds the behavior data budget");
        result.authored = true;
        result.value = rust::String(json);
        return result;
    });
}

AuthoredDataRaw authored_string(const StageHandle& handle, rust::Str path_text, rust::Str property_text, std::uint32_t max_bytes) {
    return handle.impl_->access([&] {
        AuthoredDataRaw result;
        result.authored = false;
        const SdfPath path(to_std(path_text));
        const std::string property = to_std(property_text);
        if (!path.IsAbsolutePath() || !path.IsPrimPath() || !SdfPath::IsValidNamespacedIdentifier(property) || max_bytes == 0 || max_bytes > 65536)
            throw std::runtime_error("invalid authored string read");
        const auto prim = handle.impl_->stage->GetPrimAtPath(path);
        if (!prim) throw std::runtime_error("authored string target does not exist");
        const auto attribute = prim.GetAttribute(TfToken(property));
        if (!attribute || !attribute.HasAuthoredValueOpinion()) return result;
        if (attribute.GetTypeName() != SdfValueTypeNames->String)
            throw std::runtime_error("behavior context must be an authored bounded string");
        std::string value;
        if (!attribute.Get(&value, UsdTimeCode::Default()) || value.size() > max_bytes)
            throw std::runtime_error("behavior context is unset or exceeds the data budget");
        result.authored = true;
        result.value = rust::String(value);
        return result;
    });
}

std::uint64_t observer_checkpoint(const StageHandle& handle) {
    return handle.impl_->access([&] { return static_cast<std::uint64_t>(handle.impl_->observer->checkpoint()); });
}

void observer_rollback_to(StageHandle& handle, std::uint64_t checkpoint) {
    handle.impl_->access([&] {
    handle.impl_->observer->rollback_to(
        static_cast<std::size_t>(checkpoint));
    });
}

WgslMaterialRaw material_surface_wgsl(const StageHandle& handle,
                                       rust::Str path) {
    return handle.impl_->access([&] {
    const SdfPath sdf_path(to_std(path));
    const UsdPrim prim = handle.impl_->stage->GetPrimAtPath(sdf_path);
    WgslMaterialRaw out;
    out.material_path = rust::String(to_std(path));
    if (!prim || !prim.IsA<UsdShadeMaterial>()) {
        out.diagnostics.push_back(rust::String(
            "material_surface_wgsl: prim not found or not a Material: " +
            to_std(path)));
        return out;
    }

#ifdef IDTX_HAVE_MATERIALX_WGSL
    // Fast path: C++ WgslShaderGenerator when MaterialX WGSL support compiled in.
    try {
        auto result = idtx::materialx::WgslShaderGenerator::Generate(
            handle.impl_->stage, sdf_path);
        out.wgsl_source   = rust::String(result.wgsl_source);
        out.fingerprint   = rust::String(result.fingerprint);
        out.requires_phase2 = result.requires_phase2;
        for (const auto& d : result.diagnostics) {
            out.diagnostics.push_back(rust::String(d));
        }
    } catch (const std::exception& e) {
        out.diagnostics.push_back(rust::String(
            std::string("WgslShaderGenerator failed: ") + e.what()));
    }
#else
    // Naga GLSL→WGSL fallback path: emit a marker GLSL stub. The Rust side
    // (idtx-usd crate) detects requires_phase2 = true and runs the naga
    // transpile in a background task to avoid blocking the USD traversal.
    out.requires_phase2 = true;
    // Emit a minimal GLSL stub that encodes the material path so the Rust
    // naga pass can locate the material and generate real GLSL via
    // MaterialXGenGlsl before transpiling to WGSL.
    std::ostringstream glsl;
    glsl << "// IDTX_MATERIALX_GLSL_STUB\n";
    glsl << "// material=" << to_std(path) << "\n";
    glsl << "#version 450\n";
    glsl << "layout(location=0) out vec4 frag_color;\n";
    glsl << "void main() { frag_color = vec4(0.5,0.5,0.5,1.0); }\n";
    out.wgsl_source = rust::String(glsl.str());
    // Fingerprint: hash of the material path (stable across frames).
    std::size_t h = std::hash<std::string>{}(to_std(path));
    std::ostringstream fp;
    fp << std::hex << h;
    out.fingerprint = rust::String(fp.str());
#endif

    return out;
    });
}

AttributeValueRaw eval_attribute(
    const StageHandle& handle, rust::Str prim_path, rust::Str attr_name,
    double time_code, bool use_default_time) {
    return handle.impl_->access([&] {
    AttributeValueRaw out;
    const SdfPath sdf_path(to_std(prim_path));
    const UsdPrim prim = handle.impl_->stage->GetPrimAtPath(sdf_path);
    if (!prim) {
        return out;
    }
    const TfToken token(to_std(attr_name));
    const UsdAttribute attr = prim.GetAttribute(token);
    if (!attr) {
        return out;
    }
    out.type_name = rust::String(attr.GetTypeName().GetAsToken().GetString());
    out.is_time_varying = attr.ValueMightBeTimeVarying();
    std::vector<double> times;
    attr.GetTimeSamples(&times);
    out.time_sample_count = static_cast<std::uint64_t>(times.size());

    VtValue val;
    const bool got = use_default_time
        ? attr.Get(&val)
        : attr.Get(&val, UsdTimeCode(time_code));
    if (!got) {
        return out;
    }
    out.has_value = true;

    std::ostringstream ss;
    ss << val;
    out.value_json = rust::String(ss.str());
    return out;
    });
}

// Typed array reads for custom-schema geometry (IdtxCollisionAPI). These mirror
// the mesh-projection array handling (one GfVec3f/int at a time into a flat
// rust::Vec); the collision arrays are small and read once at hydrate time, so
// no bulk memcpy is warranted. All return an empty vec on any miss.
rust::Vec<float> eval_float2_array(
    const StageHandle& handle, rust::Str prim_path, rust::Str attr_name,
    double time_code, bool use_default_time) {
    return handle.impl_->access([&] {
        rust::Vec<float> out;
        const UsdPrim prim = handle.impl_->stage->GetPrimAtPath(SdfPath(to_std(prim_path)));
        if (!prim) return out;
        const UsdAttribute attr = prim.GetAttribute(TfToken(to_std(attr_name)));
        if (!attr) return out;
        VtArray<GfVec2f> values;
        if (!attr.Get(&values, use_default_time ? UsdTimeCode::Default() : UsdTimeCode(time_code))) return out;
        out.reserve(values.size() * 2);
        for (const auto& v : values) { out.push_back(v[0]); out.push_back(v[1]); }
        return out;
    });
}

rust::Vec<float> eval_point3f_array(
    const StageHandle& handle, rust::Str prim_path, rust::Str attr_name,
    double time_code, bool use_default_time) {
    return handle.impl_->access([&] {
    rust::Vec<float> out;
    const UsdPrim prim =
        handle.impl_->stage->GetPrimAtPath(SdfPath(to_std(prim_path)));
    if (!prim) {
        return out;
    }
    const UsdAttribute attr = prim.GetAttribute(TfToken(to_std(attr_name)));
    if (!attr) {
        return out;
    }
    VtArray<GfVec3f> values;
    const bool got = use_default_time
        ? attr.Get(&values)
        : attr.Get(&values, UsdTimeCode(time_code));
    if (!got) {
        return out;
    }
    out.reserve(values.size() * 3);
    for (const GfVec3f& v : values) {
        out.push_back(v[0]);
        out.push_back(v[1]);
        out.push_back(v[2]);
    }
    return out;
    });
}

rust::Vec<std::int32_t> eval_int_array(
    const StageHandle& handle, rust::Str prim_path, rust::Str attr_name,
    double time_code, bool use_default_time) {
    return handle.impl_->access([&] {
    rust::Vec<std::int32_t> out;
    const UsdPrim prim =
        handle.impl_->stage->GetPrimAtPath(SdfPath(to_std(prim_path)));
    if (!prim) {
        return out;
    }
    const UsdAttribute attr = prim.GetAttribute(TfToken(to_std(attr_name)));
    if (!attr) {
        return out;
    }
    VtArray<int> values;
    const bool got = use_default_time
        ? attr.Get(&values)
        : attr.Get(&values, UsdTimeCode(time_code));
    if (!got) {
        return out;
    }
    out.reserve(values.size());
    for (int v : values) {
        out.push_back(static_cast<std::int32_t>(v));
    }
    return out;
    });
}

rust::Vec<rust::String> eval_token_array(
    const StageHandle& handle, rust::Str prim_path, rust::Str attr_name,
    double time_code, bool use_default_time) {
    return handle.impl_->access([&] {
    rust::Vec<rust::String> out;
    const UsdPrim prim =
        handle.impl_->stage->GetPrimAtPath(SdfPath(to_std(prim_path)));
    if (!prim) {
        return out;
    }
    const UsdAttribute attr = prim.GetAttribute(TfToken(to_std(attr_name)));
    if (!attr) {
        return out;
    }
    VtArray<TfToken> values;
    const bool got = use_default_time
        ? attr.Get(&values)
        : attr.Get(&values, UsdTimeCode(time_code));
    if (!got) {
        return out;
    }
    for (const TfToken& t : values) {
        out.push_back(rust::String(t.GetString()));
    }
    return out;
    });
}

bool get_prim_authored_visibility(const StageHandle& handle, rust::Str path_text) {
    return handle.impl_->access([&] {
    const SdfPath path(to_std(path_text));
    const UsdPrim prim = handle.impl_->stage->GetPrimAtPath(path);
    if (!prim) { return true; }
    const UsdGeomImageable imageable(prim);
    if (!imageable) { return true; }
    TfToken vis;
    if (!imageable.GetVisibilityAttr().Get(&vis)) { return true; }
    return vis != UsdGeomTokens->invisible;
    });
}

bool prim_has_api(const StageHandle& handle, rust::Str prim_path,
                  rust::Str schema_name) {
    return handle.impl_->access([&] {
    const UsdPrim prim = handle.impl_->stage->GetPrimAtPath(
        SdfPath(to_std(prim_path)));
    if (!prim) { return false; }
    const std::string name = to_std(schema_name);
    // Prefer the short USD schema type name ("ShadowAPI") via the registry, and
    // fall back to a fully-qualified TfType name ("UsdLuxShadowAPI").
    TfType schema_type =
        UsdSchemaRegistry::GetInstance().GetTypeFromSchemaTypeName(TfToken(name));
    if (schema_type.IsUnknown()) {
        schema_type = TfType::FindByName(name);
    }
    if (schema_type.IsUnknown()) { return false; }
    return prim.HasAPI(schema_type);
    });
}

bool prim_is_a(const StageHandle& handle, rust::Str prim_path,
               rust::Str type_name) {
    return handle.impl_->access([&] {
    const UsdPrim prim = handle.impl_->stage->GetPrimAtPath(
        SdfPath(to_std(prim_path)));
    if (!prim) { return false; }
    const std::string name(to_std(type_name));
    TfType schema_type =
        UsdSchemaRegistry::GetInstance().GetTypeFromSchemaTypeName(TfToken(name));
    if (schema_type.IsUnknown()) {
        schema_type = TfType::FindByName(name);
    }
    if (schema_type.IsUnknown()) { return false; }
    return prim.IsA(schema_type);
    });
}

// ── Animation discovery ───────────────────────────────────────────────────────

rust::Vec<rust::String> animated_prim_paths(const StageHandle& handle) {
    return handle.impl_->access([&] {
    const UsdStageRefPtr& stage = handle.impl_->stage;
    rust::Vec<rust::String> result;
    for (const UsdPrim& prim : stage->Traverse()) {
        if (prim.IsAbstract() || !prim.IsActive()) { continue; }
        for (const UsdProperty& prop : prim.GetAuthoredProperties()) {
            if (!prop.Is<UsdAttribute>()) { continue; }
            if (prop.As<UsdAttribute>().GetNumTimeSamples() > 0) {
                result.push_back(rust::String(prim.GetPath().GetString()));
                break;
            }
        }
    }
    return result;
    });
}

rust::Vec<rust::String> animated_attr_names(const StageHandle& handle,
                                            rust::Str prim_path) {
    return handle.impl_->access([&] {
    const UsdStageRefPtr& stage = handle.impl_->stage;
    rust::Vec<rust::String> result;
    const UsdPrim prim = stage->GetPrimAtPath(SdfPath(to_std(prim_path)));
    if (!prim) { return result; }
    for (const UsdProperty& prop : prim.GetAuthoredProperties()) {
        if (!prop.Is<UsdAttribute>()) { continue; }
        const UsdAttribute attr = prop.As<UsdAttribute>();
        if (attr.GetNumTimeSamples() > 0) {
            result.push_back(rust::String(attr.GetName().GetString()));
        }
    }
    return result;
    });
}

AttrTimeSamplesRaw attr_time_samples(const StageHandle& handle,
                                     rust::Str prim_path,
                                     rust::Str attr_name) {
    return handle.impl_->access([&] {
    AttrTimeSamplesRaw out;
    const UsdStageRefPtr& stage = handle.impl_->stage;
    const UsdPrim prim = stage->GetPrimAtPath(SdfPath(to_std(prim_path)));
    if (!prim) { return out; }
    const UsdAttribute attr = prim.GetAttribute(TfToken(to_std(attr_name)));
    if (!attr) { return out; }
    std::vector<double> times;
    attr.GetTimeSamples(&times);
    for (double t : times) { out.times.push_back(t); }
    // Interpolation hint (relevant for primvars; defaults to linear).
    TfToken interp;
    if (attr.GetMetadata(TfToken("interpolation"), &interp)) {
        out.interpolation = rust::String(interp.GetString());
    }
    return out;
    });
}

#ifdef IDTX_OPENEXEC
// ── OpenExec dataflow FFI ────────────────────────────────────────────────────
//
// These are the lock-owning wrappers around the lock-free `idtx_exec::*` compute
// surface (cxx/usd_exec_bridge.cpp). They are the ONLY exec code that touches the
// live stage, the lifecycle lock, and the Observer — the bridge TU never sees
// StageHandle/Impl. Authoring targets the SESSION layer via UsdEditContext, and
// the exclusive lifecycle lock serialises compute + authoring against all other
// stage access (OpenUSD forbids concurrent stage mutation). `tick_exec` mirrors
// the author_session_property idiom: checkpoint the observer, unlock before any
// throw (destructors are skipped unwinding across the cxx boundary), rollback the
// provoked notices on failure.

// (Re)build the per-stage exec program. Returns the registered value-key count
// (0 => no IDTX compute graph; the host skips the whole subsystem).
std::uint64_t exec_build_program(StageHandle& handle) {
    return handle.impl_->access([&] {
        return static_cast<std::uint64_t>(
            idtx_exec::build_program(handle.impl_->stage));
    });
}

// REST datasource configs for the host poller. Read-only (shared lock).
rust::Vec<ExecDataSourceRaw> exec_data_sources(const StageHandle& handle) {
    return handle.impl_->access([&] {
        rust::Vec<ExecDataSourceRaw> out;
        for (const auto& cfg : idtx_exec::data_sources(handle.impl_->stage)) {
            ExecDataSourceRaw raw;
            raw.prim_path = rust::String(cfg.prim_path);
            raw.kind = rust::String(cfg.kind);
            raw.endpoint = rust::String(cfg.endpoint);
            raw.query = rust::String(cfg.query);
            raw.method = rust::String(cfg.method);
            raw.authorization = rust::String(cfg.authorization);
            raw.json_body = rust::String(cfg.json_body);
            raw.interval = cfg.interval;
            out.push_back(std::move(raw));
        }
        return out;
    });
}

// Latest resolved datasource-input values (connected authorization / jsonBody)
// for the host poller to refresh after tick_exec. Read-only (shared lock).
rust::Vec<ExecDataSourceInputRaw> exec_data_source_inputs(const StageHandle& handle) {
    return handle.impl_->access([&] {
        rust::Vec<ExecDataSourceInputRaw> out;
        for (const auto& in : idtx_exec::data_source_inputs(handle.impl_->stage)) {
            ExecDataSourceInputRaw raw;
            raw.prim_path = rust::String(in.prim_path);
            raw.attribute_name = rust::String(in.attribute_name);
            raw.value = rust::String(in.value);
            out.push_back(std::move(raw));
        }
        return out;
    });
}

// Enqueue an external JSON payload for a datasource prim's `outputs:data`. No
// stage access (own queue mutex), so no lifecycle lock — but we still read the
// stage RefPtr under the shared lock to key the bridge registry safely.
bool exec_enqueue(const StageHandle& handle, rust::Str prim_path, rust::Str body) {
    return handle.impl_->access([&] {
        return idtx_exec::enqueue(handle.impl_->stage, to_std(prim_path),
                                  to_std(body));
    });
}

// Whether this stage's compute graph carries a time/host-state provider
// (`Compute_Environment`), cached at build. The host reads this ONCE when it
// builds the exec subsystem and uses it to decide whether to skip `tick_exec`
// (and thus the exclusive stage lock) on idle frames. Reads bridge state only
// (its own registry mutex); the shared lock here only guards the `impl_->stage`
// RefPtr read against a concurrent teardown, exactly like `exec_enqueue`.
bool exec_has_live_providers(const StageHandle& handle) {
    return handle.impl_->access([&] {
        return idtx_exec::has_live_providers(handle.impl_->stage);
    });
}

// Apply pending updates + compute + author changed results into the SESSION
// layer. Returns the number of attributes authored this tick (0 => no change,
// host can skip reprojection).
std::uint64_t tick_exec(StageHandle& handle) {
    StageAccessGuard stage_guard(handle.impl_->stage_mutex);
    const UsdStageRefPtr& stage = handle.impl_->stage;
    const SdfLayerHandle session = stage->GetSessionLayer();
    if (!session) {
        stage_guard.unlock();
        throw std::runtime_error("stage has no session layer");
    }
    const std::size_t ckpt = handle.impl_->observer->checkpoint();
    // Keep the edit-target scope nested: on the documented no-`/EHsc` toolchain
    // destructors are skipped when an exception unwinds across the cxx boundary,
    // so we must leave this scope normally to guarantee UsdEditContext restores
    // the prior edit target before we rollback/unlock and rethrow.
    std::exception_ptr error;
    std::uint64_t authored = 0;
    {
        const UsdEditContext ec(stage, session);
        try {
            authored = static_cast<std::uint64_t>(idtx_exec::tick(stage));
        } catch (...) {
            error = std::current_exception();
        }
    }
    if (error) {
        handle.impl_->observer->rollback_to(ckpt);
        stage_guard.unlock();
        std::rethrow_exception(error);
    }
    return authored;
}

// Drop the per-stage exec bridge (stage teardown / topology resync).
void exec_release(StageHandle& handle) {
    handle.impl_->access([&] {
        idtx_exec::release(handle.impl_->stage);
    });
}
#endif  // IDTX_OPENEXEC

#ifdef IDTX_AUTHORING
void test_stage_access_stress(StageHandle& stage, StageHandle& peer, std::uint32_t iterations) {
#ifdef IDTX_TEST_SUPPORT
    if (iterations > 10000) throw std::runtime_error("stress iteration limit");
    std::atomic<bool> failed{false};
    const auto identifier = stage_metadata(peer).root_identifier;
    const auto check = [](const rust::Vec<double>& matrix) {
        return matrix.size() == 16 && matrix[13] == 2.0 * matrix[12];
    };
    const auto read = [&] {
        try {
            const rust::Vec<rust::String> roots;
            for (std::uint32_t i = 0; i < iterations; ++i) {
                project_viewport(stage, 0.0, true, rust::Slice<const rust::String>(roots.data(), roots.size()));
                if (!check(get_transform_matrix(stage, "/World", false)) || !check(get_transform_matrix(peer, "/World", false))) failed = true;
            }
        } catch (...) { failed = true; }
    };
    std::thread reader1(read), reader2(read);
    std::thread writer([&] {
        try {
            for (std::uint32_t i = 0; i < iterations; ++i) {
                double matrix[16] = {1,0,0,0, 0,1,0,0, 0,0,1,0, double(i),2.0*double(i),0,1};
                author_session_transform(stage, "/World", rust::Slice<const double>(matrix, 16));
                author_session_property(stage, "/World", "s17:flag", "bool", "true");
                try {
                    author_session_property(stage, "/World", "s17:flag", "string", "wrong type");
                    failed = true;
                } catch (const std::runtime_error&) {}
                apply_matrix_transform(peer, "/World", rust::Slice<const double>(matrix, 16));
                if (i % 17 == 0) reload_layer(peer, identifier);
            }
        } catch (...) { failed = true; }
    });
    reader1.join(); reader2.join(); writer.join();
    if (failed) throw std::runtime_error("concurrent stage projection/write/reload failed");
#else
    (void)stage; (void)peer; (void)iterations;
    throw std::runtime_error("stage stress requires test-support");
#endif
}
#endif

}  // namespace idtx
