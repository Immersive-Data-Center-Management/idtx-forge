# Bounded native PointInstancer updates (F5)

`stage_edit` has two typed deferred commands:

```rust
let ticket = ctx.stage_edit().set_point_instances(
    ctx.entity(),
    &[
        PointInstance { id: 77, prototype_index: 0,
            position: [1.0, 0.0, 0.0], ..PointInstance::default() },
        PointInstance { id: -9, prototype_index: 1,
            position: [2.0, 0.0, 0.0], ..PointInstance::default() },
    ],
    &[-9],
)?;
// On subsequent frames, consume the terminal result:
let result = ctx.stage_edit().edit_status(ticket)?;
// status: 0 Accepted, 1 Committed, 2 Failed; error is the ABI ErrorCode.
// Once placement has committed, reveal/hide by native id:
let pulse = ctx.stage_edit().set_point_instancer_visibility(ctx.entity(), &[77])?;
```

See [the allocation-free visibility-window example](examples/behaviors/instancer/visibility_window.rs).

## Contract

- Target an **existing, loaded `UsdGeomPointInstancer`** with resolved native
  `prototypes`. Maximum **4096 entries / 256 prototypes** per bounded update.
  The SDK's single typed record fixes the relative lengths of positions,
  orientations, scales, ids and prototype indices. Native array callers also
  validate every length explicitly. Values must be finite; rotations are **unit
  XYZW quaternions**, converted to USD's real-first `quath` representation.
  If the native prim already uses higher-precision `orientationsf`, it is updated
  in the same transaction, honoring USD's precedence rule.
- Position and scale values use **instancer-local USD coordinates/units**, not
  render-space metres. Native `ComputeInstanceTransformsAtTime(IncludeProtoXform)`
  supplies prototype-root transforms. Extraction composes the instancer's world
  transform and the mesh's prototype-relative transform exactly once.
- `id` is a unique signed 64-bit identity. Reordered, negative and noncontiguous
  ids are valid. A full replacement explicitly reallocates the pool; retained
  ids retain identity. `invisible_ids` must be unique existing **ids**, not slots.
  Native implicit index ids are readable/maskable until an explicit allocation.
- Both commands author native **session-layer defaults**, at the existing
  after-hooks deferred boundary. Each pulse replaces `invisibleIds`; it does
  not append time samples or modify list-editable `inactiveIds` metadata. Native
  authored time-varying masks still evaluate through `ComputeMaskAtTime`.
  The rolling strongest-layer opinion remains inspectable via the inspector or
  `StageService::session_layer_text()`; root-layer Save does not persist pulses.
- Validation precedes writes. All fields commit within one native change block;
  failure restores prior defaults, time samples, absent specs and notices. Tests
  inject failure after every field write. No partially updated array set survives.
- A successful enqueue returns a **store-owned ticket**, not a claim that the
  edit committed. Schema/liveness/prototype checks repeat at commit. Results are
  one-shot when terminal; Accepted reads retain the ticket. At most 64 tickets
  may be outstanding per behavior. Consume them. Unload/replacement/stop cancels
  pending work; unknown/consumed tickets return `NotFound`, and surviving owners
  can read the cancellation error. Ticket ids are never reused by replacement stores.
- Existing per-behavior and host queue budgets apply. The wire placement payload
  is `56 × count + 8 × hidden_count` bytes; mask-only is `8 × hidden_count`.
  The default argument budget is **64 KiB**, so use an explicitly larger
  behavior-work `argument_bytes` budget (e.g. **512 KiB**) for the maximum pool.
  Retained queue/result overhead is also charged. Oversized input and invalid
  output pointers are rejected before allocation/enqueue.

Conductor's **Project Settings → Behavior → Argument / command cap** controls
the override. It is saved and cooked into the shared runtime configuration:

```toml
[behavior.limits]
argument_bytes = 524288 # 512 KiB; zero/omitted keeps the existing 64 KiB default
```

The F5 placement/mask commands provide **binary visibility** and always write
native inspectable opinions. Continuous coverage is provided separately by the
frame-local rendering API below. Instances remain native ids, not USD prims or
entities; `highlight(instanceEntity)` is not provided.

## Continuous presence opacity (2026-11-25)

Native `primvars:displayOpacity` is supported on PointInstancers: a constant
float array of length one, or a vertex float array parallel to the allocation.
Indexed primvars are flattened natively. Absent values mean coverage one;
invalid interpolation/length/nonfinite/out-of-range values are diagnosed and
hidden. For a zero-start effect, author:

```usda
float[] primvars:displayOpacity = [0] (interpolation = "constant")
```

After consuming the F5 placement commit ticket, a behavior with the `overlay`
capability can override coverage for existing **native ids**:

```rust
ctx.render().set_point_instance_opacities(instancer, &[
    PointInstanceOpacity { id: 77, opacity: 0.37, reserved: 0 },
])?;
```

- One bounded call, at most 4096 unique existing ids; finite `[0,1]` values.
  The wire record is 16 bytes. Capability, memory/argument/queue bounds, schema,
  duplicate ids and live membership are checked before enqueueing.
- Values last one behavior frame. Post again to hold an effect (including a
  zero-delta update). Missing ids return to authored opacity; a missing command
  restores the whole instancer's authored values. Last batch for an owner wins.
  Native `invisibleIds`, `inactiveIds` and inherited visibility still apply.
- This API uses the existing transient render-command channel, **not USD edits**.
  Source geometry, bindings and defaults remain authored in USD. Placement edits
  preserve primvars; prefer constant defaults when reallocating variable-length
  pools, or keep vertex arrays aligned with the allocation.
- Coverage opts the batch into a stable presence/blend pipeline. It multiplies
  the resolved shaded response, including emission, exactly once. Use a shared
  `UsdPreviewSurface` presence material with opacity 1 for direct coverage values.
- Zero-coverage entries are compacted out. The prototype/material stays resident
  even when all entries are zero. Warm frames patch native instance buffers,
  without USD projection, topology changes or material-binding rebuilds.
- Desktop and externally hosted XR commands share the same resolution path.
  Stop/unload clears overrides; `point_instance_opacity_overrides(path)` exposes
  the current frame's overrides read-only for diagnostics. These are separate
  from the inspector's authored/native mask values.

`point_instance_opacity_has_linear_coverage_and_warm_resource_reuse` in
`idtx-render/tests/opacity_presence.rs` verifies actual linear-HDR coverage,
exactly-once RGB premultiplication, zero culling and all-hidden/reveal resource
reuse. The session's `point_instancer::tests::opacity_tests` covers the real WASM
boundary and warm frames with no USD/material/topology churn.

## Rendering and Conductor

- A dedicated `INSTANCER` dirty domain and narrow native sampler avoid stage
  traversal, mesh/material extraction and topology projection for these edits.
  Mask-only sampling omits transform computation and matrix marshalling.
  Initially overriding time-sampled arrays also reconciles animation metadata
  once, so obsolete tracks cannot force full extraction during subsequent play.
- For bounded pools the ECS retains all placement slots, ids and the native mask.
  Large static DCC pools keep the compact visible-only projection. The snapshot
  caches prototype meshes/materials and unmasked placement matrices, compacting
  only visible instance buffers. Fully hidden prototype batches stay resident.
  Native identity is carried separately as `(instancer path, id)`; no fake prim
  paths are added to `instance_prim_paths`.
- Native buffer versions are separate from the structural snapshot serial and
  other instance versions. Pulses update only affected native instance buffers.
  Static single-instance merging remains available; runtime-updated native
  singletons stay out of that merger to avoid window-induced rebakes.
  Mixed mask/steady-state transform edits retain existing warm patch paths.
  Prim framing bounds are refreshed from cached prototype AABBs and placements.
- Select an instancer in Conductor for prototypes, ids, local arrays, effective
  masks, invalid-entry diagnostics, optional placement guides, and a bounded
  geometry/memory report. Entry editing is paged. Derived play values are
  read-only. **Apply authored defaults** uses normal root-layer Save/Undo.
- Generate through **F2's Bake placements along curve**, then choose that baked
  `Placements` output under **Use F2 baked placements**. Import converts those
  native Xforms into local instancer arrays. It is an explicit copy: rebaking F2
  requires reimporting. Sheared/singular TRS conversions are rejected. Prototype
  references remain ordinary USD/cook dependencies; no parallel asset registry
  is introduced. Static instancers larger than the runtime limit remain loadable.

## Verification / measurements

Run with `IDTX_USD_DIR` and the platform's native-library loader path configured:

```sh
cargo test -p idtx-usd -p idtx-behavior -p idtx-studio-session \
  --features idtx-usd/test-support,idtx-studio-session/authoring,idtx-studio-session/metrics \
  point_instancer -- --include-ignored --test-threads=1 --nocapture
```

The tests cover mixed mesh/gprim prototypes, referenced parents and transformed
prototypes, reordered ids, both native masks, invalid writes/rollback, wasm bounds
and capability gating, ticket cancellation, and repeated all-hidden/reveal windows.
GPU counters verify no unchanged prototype upload or topology growth; geometry
Arcs and snapshot serials remain stable.

Initial macOS/Apple-silicon dev-profile measurement (120 warm pulses, 32 visible,
CPU native author + notice/hydration + snapshot preparation; excludes guest
marshalling and GPU time):

| Pool | p50 | p95 |
|---|---:|---:|
| PointInstancer, 256 allocated | 43 µs | 54 µs |
| PointInstancer, 1024 allocated | 156 µs | 172 µs |
| PointInstancer, 4096 allocated | 628 µs | 667 µs |
| Reused standalone mesh pool, 32 allocated | 122 µs | 136 µs |

The baseline has 32 small, slightly different cubes so automatic instance
promotion does not replace the mesh-pool strategy being measured. These are
local measurements, not portable frame-time guarantees. Native mask validation
and evaluation scale with allocated count. A small visible window over thousands
of allocations can favor the small mesh pool: **keep F5 optional for the hunt**
until the target scene/device measurements justify that dependency.
