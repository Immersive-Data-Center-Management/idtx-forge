# idtx-usd-extension

OpenUSD schema plugins for IDTX — custom `IsA` / `HasAPI` schemas that let
`idtx:`-namespaced content validate (apiSchemas, allowedTokens) and, in later
iterations, drive hydration behavior in the C++ bridge and the Rust scene/render
layers.

## Collision: OpenUSD-standard schemas + reduced `IdtxCollisionAPI` addendum

Collidability is authored with the **OpenUSD-standard** physics schemas, so any
usdPhysics-aware DCC reads a valid *static* collider with zero IDTX knowledge:

| schema (applied to the gprim) | authored properties |
|---|---|
| `UsdPhysicsCollisionAPI` | `physics:collisionEnabled` (bool, default `true`) — the collidable gate |
| `UsdPhysicsMeshCollisionAPI` (meshes only) | `physics:approximation` (`none` / `convexHull` / `boundingSphere` / `boundingCube` / `convexDecomposition` / `meshSimplification`) |

`IdtxCollisionAPI` is now a **reduced, codeless addendum** applied *alongside* the
standard schemas, carrying only what the standard cannot persist. A codeless
schema compiles no C++; it ships as pure resource data —
[`resources/plugInfo.json`](resources/plugInfo.json) plus
[`resources/generatedSchema.usda`](resources/generatedSchema.usda) — registered at
runtime via `PlugRegistry::RegisterPlugins`, so the existing bridge calls
(`prim_has_api`, `eval_attribute`, `StructuralCommand::ApplyApi`) recognise it
with no `idtx-usd-sys` changes. It adds three attributes in the `idtx:collision`
namespace:

| attribute                        | type        | values / meaning |
|----------------------------------|-------------|------------------|
| `idtx:collision:hull:points`     | `point3f[]` | pre-baked convex-hull vertices (local space; used when `physics:approximation = convexHull`, never regenerated at runtime) |
| `idtx:collision:hull:triangles`  | `int[]`     | flat triangle index list into `hull:points` |
| `idtx:collision:channel`         | `token[]`   | IDTX query domains — `All` `Physics` `Interaction` (no USD-standard equivalent) |

The **resolved** collider shape and its analytic dimensions are derived *at
hydration* (`idtx_scene::collision::resolve_collision_shape` + the collider
gprim's own `radius`/`size`/`height`/`axis` geometry), not from authored tokens:

- `physics:approximation = convexHull` → the pre-baked hull (`ray_trisoup`);
- `boundingSphere`/`boundingCube` → analytic sphere/box sized from the mesh bounds;
- `none` (or no `UsdPhysicsMeshCollisionAPI`) → the gprim type selects the analytic
  shape (`Sphere`/`Cube`/`Cylinder`/`Cone`/`Capsule`), else the mesh itself.

This feeds `idtx_scene::CollisionComp`, the picking raycast narrow-phase
(`idtx_scene::picking`, incl. the gprim `axis` orientation), and the editor
collision wireframe (`idtx_render::push_collision_analytic`). A `convexHull`
collider with no authored hull data is treated as **non-colliding** (IDTX never
cooks a hull at runtime).

> The legacy single-schema vocabulary (`idtx:collision:enabled`/`:shape`/
> `:<shape>:*`) is sunset — there is no read shim; old-vocab fixtures are migrated.

## Usage

```rust
// Call ONCE, before opening any stage that uses the schema.
idtx_usd_extension::install()?;
```

## Files

- `resources/` — what the USD runtime actually reads. `build.rs` copies these
  next to each other into `OUT_DIR/idtx-usd-extension/`; `plugin_dir()` finds
  that (or the bundled `Contents/Resources/idtx-usd-extension/` in a `.app`).
- `schema/schema.usda` — the usdGenSchema **source**. The runtime ignores it; it
  documents intent and regenerates the resources. On a host with a Python-enabled
  OpenUSD (`$IDTX_USD_GEN_DIR`, e.g. `thirdparty/openusd-26.08-withPython`):

  ```bash
  cargo xtask gen-schema --crate idtx-usd-extension \
    --usd-gen-dir <python-enabled-usd-prefix>
  ```

  Regeneration gotchas (all handled if you follow these):
  - The source `subLayers` must include `@usdGeom/schema.usda@` so bases like
    `Gprim` / `Imageable` resolve (the abstract geom bases alone resolve without
    it, but the concrete `Gprim` does not).
  - Set `PYTHONUTF8=1` — a non-ASCII char in a doc string (e.g. `∈`) otherwise
    trips the Windows `charmap` codec while writing `generatedSchema.usda`.
  - usdGenSchema prepends a `#` comment header to `plugInfo.json`; strip it so the
    file stays valid JSON for the unit tests (the USD runtime tolerates it, serde
    does not). The `@PLUG_INFO_*@` path placeholders are preserved by the xtask.
  - If usdGenSchema re-adds a per-type `"alias"` block, strip it (see the note
    below).

  Where usdGenSchema is unavailable, the resources can be hand-maintained in sync.

  **Schema hierarchy:** `IdtxWidgetPanel` derives from `UsdGeomGprim` — the one
  widget prim that enters real 3D space (rendered as a quad). Every element type
  (Container/Label/Button/Image/Slider/Toggle + the `IdtxWidgetField*` family)
  derives from the abstract `IdtxWidgetElement : UsdGeomImageable` — a pure visual
  composited into the panel's 2D texture, with no independent transform/bounds.
  Positioning is 2D, via `idtx:widget:position` (normalized) + `idtx:widget:order`
  on that base.

  > **Note:** `plugInfo.json` intentionally omits the per-type `"alias"` block.
  > For a codeless schema the USD schema registry adds the `IdtxCollisionAPI`
  > `TfType` alias from `generatedSchema.usda`; declaring it again in
  > `plugInfo.json` makes Plug add it a second time, which OpenUSD reports as a
  > (non-fatal) `AddAlias` coding error. If you regenerate with usdGenSchema and
  > it re-adds `"alias"`, strip it.

## Tests

- Unit tests (no USD link): token roundtrips (incl. the `Approximation` enum),
  attribute-name constants, and validation that the shipped `plugInfo.json` /
  `generatedSchema.usda` declare the reduced schema correctly.
  `cargo test -p idtx-usd-extension`.
- Integration tests (`#[ignore]`, need `IDTX_USD_DIR` + `DYLD_LIBRARY_PATH` and
  the linked usdPhysics plugin): registration, detection of both the standard
  gate and the reduced addendum, attribute evaluation, apply/remove. Run with
  `cargo test -p idtx-usd-extension -- --ignored`.
