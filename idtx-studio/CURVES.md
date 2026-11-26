# Native curves and placement baking

## Editors

In Poser or Conductor, use the scene-tree **Create → Linear Curve / Bezier
Curve** entries. These author `UsdGeomBasisCurves` with native `points`,
`curveVertexCounts`, `type`, `basis`, and `wrap`. Select the curve to inspect its
batch index, metre length, endpoints, and accuracy. Expand **Control points /
Bezier handles** to edit local coordinates, append segments, or reverse direction.
**Apply control points** is one undoable edit. Bezier handles are ordinary native
control vertices; there is no custom spline geometry schema. Control edits author
the USD default value; runtime evaluation also supports authored time samples.

Inspector displays the same tessellated overlay guides and curve information,
with no curve-authoring controls. Guides are visible in all three editors: cyan
path, green start, red end, yellow direction arrow, and selected control polygon.
They use the existing x-ray editor overlay, not a beauty/hair renderer. USD
visibility is respected. Unsupported representations retain scene-tree/property
inspection and emit diagnostics.

In Conductor's **Native curve / placements** panel:

1. Choose the explicit curve index and metre spacing/reveal window.
2. Set two alternating lateral offsets, a normalized-world floor normal, and
   full-object clearance half extents. The placement origin is on the floor;
   clearance extends upward by twice the vertical half extent. Include the
   whole cat/object, not just the printed paw. Local +Z follows the floor tangent.
3. Preview. Green footprints are clear against loaded scene bounds; red ones
   overlap. The deterministic seed selects the alternating-offset phase.
4. Run **Bake placements along curve**, then save normally. Rebake after edits.

The native source's ordinary properties `idtx:curve:eligible`,
`idtx:curve:startGroup`, and `idtx:curve:endGroup` drive route eligibility and
endpoint-group diagnostics. Group labels apply to each curve in a batched prim.
Matching group endpoints are checked for agreement within the larger endpoint
tolerance; absent groups/peers are reported. Ineligible routes cannot be baked.

Clearance is conservative, using native composed world bounding boxes. It can
report false positives around rotated/concave geometry; PointInstancer bounds
cover the complete instancer. It is not a surface follower or navigation solver.
Unloaded payloads, muted layers, unresolved composition, and exhausted budgets
are **inconclusive errors**, never an empty warehouse. Bounds include native
instances; prototype-only objects are not counted as separate obstacles.

## Derived ownership and build path

The first release produces at most **4096 editable native Xforms**, under
`<source-parent>/IDTX_CurveBake_<path-hash>_<index>/Placements`. The owner is a
tool-owned `Scope`; the source remains native and independently editable. No API
schema registration is required. The owner stores:

- Native `idtx:placement:source` and `idtx:placement:output` relationships.
- Spacing, reveal interval, offsets, orientation, floor normal, footprint, seed,
  curve index and tolerance as ordinary namespaced USD attributes.
- Source and settings SHA-256 digests, source revision, and generator version.

`StageService::bake_curve_placements` builds an isolated USD layer, returns that
layer's USDA serialization (available for a build tool to export), and commits
only the owned output/settings to the current root layer. Xforms are converted
from normalized metre world space to their composed parent's native local
space. The editor journals the commit as one structural transaction. Rebaking
uses metre-local placement frames (so a normalized-world transform read has unit
scale). Geometry attached later should use that placement-local convention.
For referenced routes, source relationships identify the existing composed
owner; per-copy overrides replace transforms and deactivate obsolete inherited
samples, leaving the referenced asset untouched. Native instanceable parents
must be made non-instanceable before authoring child placements. Rebaking
preserves other owner children/properties and refuses to overwrite another
owner/source. Normal save/reopen preserves the native source, ownership, and
settings. The generated Scope lives inside the source's parent so referencing
that parent includes the placements.

Preview and cook detect stale source/settings digests; cook and runtime-preview
launch also revalidate loaded-scene clearance. A stale bake stops the build with
the source/owner diagnostic: open that source in Conductor, run **Bake placements
along curve**, save, and cook again. Programmatic builds can use
`preview_curve_placements`, `bake_curve_placements`, and `validate_curve_bakes`.
Validation never rewrites source assets. Bakes use **USD default time**; runtime
queries use the currently projected time sample.

## Evaluation contract

- Linear and cubic **Bezier**, nonperiodic and periodic, are supported. Linear
  curves require at least two vertices. Cubic nonperiodic Bezier requires
  **3n+1** vertices per curve; periodic requires **3n**. Counts must exactly
  partition the points. B-spline, Catmull-Rom, pinned wrap, NURBS and Hermite are
  explicitly unsupported in this release.
- C++ reads through `UsdGeomBasisCurves`, `UsdGeomXformCache`, and stage metrics.
  Typed records cross `idtx-usd-sys → idtx-usd → idtx-scene`. Rust behaviors never
  parse USDA. Scalar `TsSpline` and `UsdGeomMotionAPI` are not spatial routes.
- Distances and lengths are **metres after the full composed transform**.
  Normalized stage-world is Y-up; Z-up conversion is the same -90° X rotation
  used by the viewport. Local output retains authored local units. Nonuniform
  scales and independent reference placements have distinct world tables.
- `idtx:curve:toleranceMetres` defaults to **0.001 m**, accepted range
  `1e-9..1 m`. Adaptive de Casteljau subdivision uses chord/control-polygon
  lower/upper length bounds. The sum of leaf gaps is at most tolerance, so total
  length error is at most half tolerance (apart from floating-point roundoff).
  Distance inversion uses partial-leaf length bounds and 52 bisection steps;
  it does not treat uniform parameter steps as metres. Unrepresentable world
  precision is rejected. Tests compare against an independent dense integral.
- Hard limits: 16,384 control vertices/count entries per prim, depth 20,
  16,384 table leaves per curve and per hydrated batch, 256 distances per SDK
  call, 65,536 clearance obstacles and 4,194,304 placement/obstacle pairs.
  Limits produce errors instead of lower-quality approximate answers.
- Zero-length curves return their first point, length zero, and a stable tangent.
  Repeated endpoint controls use the first valid one-sided direction; wholly
  collapsed world curves use +X. Distances clamp to `[0,length]`; callers opt
  into looping explicitly. Frame delta is not part of sampling or baking.
- Snapshot tables are keyed by generation-scoped source identity, native
  geometry, composed double transform, units/up axis, time and tolerance.
  Unchanged inputs reuse `Arc` tables. Removed/retyped prims drop the component;
  removed entities invalidate behavior handles. Transform roots containing
  curves use the double-precision native projection rather than the renderer's
  float-only matrix patch path.

## SDK (F2.2)

Requires the behavior's `scene_query` capability. These are additive ABI 3
imports; no host allocation is sized from an unchecked guest count.

```rust,ignore
use idtx_behavior_sdk::scene::{CurveSample, CurveSpace};

let curve = ctx.scene().curve(route_entity, 0)?;
let length_metres = curve.length()?;
let marker = curve.sample_distance(speed_metres_per_second * elapsed_seconds,
                                   CurveSpace::StageWorldMetres)?;
let mut samples = [CurveSample::default(); 3];
curve.sample_distances(&[0.0, length_metres * 0.5, length_metres],
                       CurveSpace::Local, &mut samples)?;
```

`CurveSample` contains double-precision `[f64; 3]` point and unit tangent arrays.
Handles resolve the current immutable scene snapshot, not live USD. Invalid
counts/spaces/pointers/nonfinite distances return `InvalidArgument`; unsupported
bases return `TypeMismatch`; deleted entities return `StaleHandle`. Short/invalid
buffers are not partially written. Native SDK calls without a host return
`NotFound`.

## Verification

Set `IDTX_USD_DIR` to the OpenUSD 26.11.1 install and the platform's dynamic
library path to its `lib/` directory, as described in README.

```sh
cargo test -p idtx-usd --features authoring --test curves
cargo test -p idtx-scene --test curves
cargo test -p idtx-behavior --test behavior_host curve_query
cargo test -p idtx-studio-session --lib curve_queries
cargo test -p idtx-build --lib cook_rejects_stale_native_curve
cargo test -p idtx-render --test render_integration native_curve_guides
cargo check -p idtx-behavior-sdk --target wasm32-unknown-unknown
cargo check -p idtx-conductor -p idtx-inspector -p idtx-poser
```

Fixtures: `tests/fixtures/usd/curves/`. GPU verification writes
`tests/renders/curves/native_curve_guides.png`. Acceptance covers straight,
multi-segment, batched, degenerate and periodic paths; distance/tangent endpoints;
nonuniform transforms; Z-up centimetre reference copies; time samples; bounded
queries; full-footprint collisions; unloaded payloads; cache reuse/invalidation;
ownership, stale bake detection, undo and save/reopen.
