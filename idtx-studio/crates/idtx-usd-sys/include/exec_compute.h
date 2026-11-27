// OpenExec dataflow bridge — lock-free compute surface.
//
// This header declares the ONLY functions the new translation unit
// `cxx/usd_exec_bridge.cpp` exposes. They are deliberately free of any
// `StageHandle` / `Impl` / lifecycle-lock / `Observer` dependency so the bridge
// TU never needs friend access or the anonymous-namespace helpers that live in
// `usd_shim.cpp`. The locking + session `UsdEditContext` + observer
// checkpoint/rollback wrapper stays in `usd_shim.cpp` (see the `exec_*` FFI
// entry points), which calls into these on the stage thread while holding the
// exclusive stage-lifecycle lock.
//
// Port of idtx-flow's `ExecBridge`/`ExecBridgeManager`
// (idtx-flow/shared/include/idtxflow/exec/ExecBridgeManager.h), minus the Godot
// result handlers and the 100 ms worker thread: idtx-studio drives `tick()` from
// the main-thread session tick, so there is no second thread mutating the stage.

#pragma once

#ifdef IDTX_OPENEXEC

#include <cstddef>
#include <cstdint>
#include <string>
#include <vector>

#include <pxr/usd/usd/stage.h>

namespace idtx_exec {

// Datasource configuration read off an IDTX datasource prim, handed to the host
// so it can drive the prim's `outputs:data` on an interval. `kind` selects how:
// `"rest"` → fetch `endpoint`(+`query`) with HTTP; `"mock"` → generate random
// JSON locally. Both feed the same `enqueue` path (HTTP/generation stays off the
// stage thread).
struct DataSourceConfig {
    std::string prim_path;
    std::string kind;          // "rest" | "mock"
    std::string endpoint;      // rest only
    std::string query;         // rest only
    std::string method;        // rest only: "GET" | "POST"
    std::string authorization; // rest only: Authorization header, empty = none
    std::string json_body;     // rest only: POST body
    float interval = 1.0f;     // poll/generate seconds
};

// (Re)build the per-stage exec program: drop any existing bridge for `stage`,
// scan every prim for attributes carrying authored connections, register each as
// an OpenExec value key, then BuildRequest + PrepareRequest + Activate. Returns
// the number of registered value keys (0 => no IDTX compute graph in this stage,
// so the host can skip the whole subsystem / spawn no REST poller). Reads the
// stage only; the caller holds the exclusive lifecycle lock.
std::size_t build_program(const pxr::UsdStageRefPtr& stage);

// REST datasource configs in `stage`, for the host poller. Read-only.
std::vector<DataSourceConfig> data_sources(const pxr::UsdStageRefPtr& stage);

// One resolved datasource INPUT value (a connected `authorization` / `jsonBody`
// driven by a `Compute_*` node), delivered to the host poller AFTER each `tick()`
// without authoring it onto the stage.
//
// This is how a value supplied by an environment provider (an auth token, …)
// reaches the REST request: the connected input attribute is registered as a
// time-dependent value key (see `build_program`), recomputed every tick, and its
// result cached for the poller to read at fetch time — mirroring idtx-flow's
// `UsdRestDatasourceNode3D::OnComputeComplete`. The value is NOT authored back
// onto the stage (a resolved bearer token must not be persisted into the session
// layer), which is why it travels this side channel rather than `outputs:*`.
struct DataSourceInput {
    std::string prim_path;      // datasource prim carrying the connected input
    std::string attribute_name; // e.g. "authorization", "jsonBody"
    std::string value;          // resolved string value from the last tick()
};

// Latest resolved datasource-input values cached by the most recent `tick()`, for
// the host to refresh its poller's live inputs after `tick_exec`. Empty when no
// datasource input carries a connection. Reads bridge state only (no stage
// access); the caller holds the shared lifecycle lock.
std::vector<DataSourceInput> data_source_inputs(const pxr::UsdStageRefPtr& stage);

// Coalescing enqueue of an external JSON payload for `prim_path`'s `outputs:data`
// attribute. Thread-safe (own queue mutex); performs NO stage access, so the
// caller need not hold the lifecycle lock. Returns false if dropped (no active
// bridge, empty body, or queue full).
bool enqueue(const pxr::UsdStageRefPtr& stage, const std::string& prim_path,
             const std::string& body);

// Apply pending external updates, run the OpenExec compute, and author every
// CHANGED computed value onto its destination attribute via `UsdAttribute::Set`.
//
// Authoring targets the stage's CURRENT edit target — the caller must have
// opened `UsdEditContext(stage, stage->GetSessionLayer())` and must hold the
// exclusive stage-lifecycle lock (both authoring and OpenExec compute touch the
// stage). Returns the number of attributes authored this tick (0 => nothing
// changed, so the host can skip reprojection).
std::size_t tick(const pxr::UsdStageRefPtr& stage);

// Whether this stage's compute graph carries a time/host-state environment
// provider (`Compute_Environment`), cached at `build_program`. `true` => every
// tick must recompute (idcm clocks / env / auth tokens change independent of
// datasource payloads); `false` => a purely datasource-driven graph the host may
// skip on frames with no pending payload. Reads bridge state only (no stage
// access, no lifecycle lock) so the host can call it on the per-frame path.
bool has_live_providers(const pxr::UsdStageRefPtr& stage);

// Drop the per-stage bridge (on stage teardown / topology resync). Idempotent.
void release(const pxr::UsdStageRefPtr& stage);

} // namespace idtx_exec

#endif // IDTX_OPENEXEC
