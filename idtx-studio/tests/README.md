# Regression fixtures

Checked-in USD inputs live in `fixtures/usd/`, grouped by subsystem (for
example, `hydration/authored_visibility.usda`). Use `support::usd_fixture` in
tests: missing inputs and unhydrated Git LFS pointers are failures, not skips.
Keep `actions/checkout` configured with `lfs: true`; locally, run `git lfs pull`.

Generated stages, WASM modules, resolver caches and packages belong in an owned
`support::scratch_dir`. Keep its guard alive until sessions and workers finish.
Never generate files inside the checked-in fixture tree. `cargo xtask gen-heavy`
writes persistent output to `target/generated-usd/`; performance reports go to
`tests/renders/`.

## Relocation regression checks

From the `idtx-studio` workspace, with OpenUSD 26.11.1 configured via
`IDTX_USD_DIR` and its runtime libraries on the loader path:

```sh
cargo test -p idtx-usd --test fixture_paths
cargo test -p idtx-studio-session --features authoring,remote --test collaboration --test collaboration_asset_budget --no-run
cargo test -p idtx-studio-session --features physics --test physics_phase1 -- --ignored --test-threads=1
cargo test -p idtx-session-orchestration --features metrics -- --include-ignored --test-threads=1
cargo test -p idtx-build --lib --tests -- --test-threads=1
cargo build -p idtx-runtime --features metrics
```

Set `IDTX_TEST_RUNTIME` to the absolute path of the built `idtx-runtime` binary
(`target/debug/idtx-runtime.exe` on Windows), then run:

```sh
cargo test -p idtx-build --test package_launch -- --ignored --test-threads=1
```

The lifecycle, transition and packaged-launch checks require a headless GPU.
The separately ignored external BehaviorRefactor cook probe requires
`IDTX_TEST_BEHAVIOR_PROJECT` to point to its `project.toml`; it is not a
checked-in regression fixture.

## Native presence opacity (F7)

With OpenUSD and its loader path configured as above:

```sh
cargo test -p idtx-usd --features authoring --lib opacity
cargo test -p idtx-usd --features authoring --lib material
cargo test -p idtx-render --lib
cargo test -p idtx-render --test opacity_presence -- --ignored --test-threads=1
cargo test -p idtx-studio-session --features authoring --lib invalidation -- --include-ignored --test-threads=1
```

`opacity_presence` reads **linear RGBA16F**, before tone mapping, over black and
white backgrounds. It recovers coverage for 0/.2/.4/.6/.8/1, verifies RGB is
premultiplied exactly once, and checks zero presence against the unobstructed
background. It varies emission/exposure and exercises static, instanced, skinned,
static UDIM and instanced UDIM shaders, scalar/diffuse-alpha textures, masks,
warm mode-only edits, and colored-glass transmission. Explicitly running these
GPU tests requires an adapter; adapter failures fail the tests.

`opacity_modes.usda` covers default/explicit modes, Material → NodeGraph interface
connections, unsupported tokens and masking. Projection tests also check FFI
validation, mode-only fingerprints, reference overrides, property-snapshot
undo/redo, save/reopen and runtime USDZ packaging. The session regression uses
`emissive_presence.usda` to check live notice → hydration → GPU updates.

The material IR is version **2**, the containing viewport projection is version
**6**, and the material uniform remains **64 bytes** (presence uses feature bit 8).
`AlphaPolicy` still selects rasterization; `OpacityMode` selects shading.

## P1 animation, widget and async-spawn regressions

The native full/fast animation samplers and Rust decoder share
`idtx-usd-sys::bridge::ffi::ViewportProjectionSchema`. The rigid-clip regression
compares their transforms and inherited visibility. The session regression compares
ECS and warm render matrices with fresh native clip samples beneath an animated
ancestor, including paused actors and reset-xform-stack descendants.

```sh
cargo test --locked -p idtx-usd --features authoring --test animation_playback -- --test-threads=1
cargo test --locked -p idtx-scene --test widget_precision -- --test-threads=1
cargo test --locked -p idtx-studio-session --features authoring,metrics --lib animation_playback -- --include-ignored --test-threads=1
cargo test --locked -p idtx-behavior --features net --test spawn_budget -- --test-threads=1
cargo test --locked -p idtx-behavior --features net,visionos --test spawn_budget -- --test-threads=1
```

`widget_precision` exercises initial hydration and live authored edits with
ordinary, negative, boundary and oversized field precisions. `spawn_budget` posts
real async WASM requests and polls their terminal statuses before delivery; pending
callbacks must retain instance/host byte and item credits and the 64-ticket bound,
including dispatch deferral and teardown.

### Transparent-mode review

OpenUSD 26.11.1 `previewSurface.glslfx` returns resolved opacity as alpha in both
modes. Studio's existing transparent branch instead raises coverage using the
luminance of reflection/emission. F7 preserves that glass behavior for regression
compatibility; only presence implements the coverage contract above. Resolving
that existing transparent-mode difference is a separate rendering change.
MaterialX transmission continues to use transparent/colored-glass semantics.

## P2 review gates

Use Rust **1.96.1**, the workspace lockfile, `wasm32-unknown-unknown`, OpenUSD
26.11.1, and a headless GPU for the session tests. The PR workflow explicitly runs
the following, including session animation/bindings/attachment on both engines:

```sh
cargo test --locked -p idtx-platform-xr --features gesture-recognition --lib
cargo test --locked -p idtx-behavior --features net --test work_limits
cargo test --locked -p idtx-behavior --features net,visionos --test work_limits
cargo test --locked -p idtx-render --lib widget_pass
cargo test --locked -p idtx-studio-shell --features authoring --lib project_settings
cargo test --locked -p idtx-studio-session --features authoring,metrics --lib animation_playback -- --include-ignored --test-threads=1
cargo test --locked -p idtx-studio-session --features authoring,metrics --lib behavior_bindings -- --include-ignored --test-threads=1
cargo test --locked -p idtx-studio-session --features authoring,metrics --lib widget_attachment_tests -- --include-ignored --test-threads=1
cargo test --locked -p idtx-usd-resolver-http -- --include-ignored --test-threads=1
cargo test --locked -p idtx-build --features cooking,usd-resolver-http --lib -- --test-threads=1
cargo test --locked -p idtx-behavior-build --test cache_contract -- --test-threads=1
```

Separate PR jobs build/link actual Quest and visionOS targets, with metrics on/off,
and independently build and launch each remote-enabled app from an unrelated
directory with loader overrides removed. `check_app_launch.py` renders a runtime
PNG; the three editor executables use their metrics-only USD startup probe
(`IDTX_STARTUP_PROBE_STAGE`) without requiring a GUI desktop. Forge's CMake gate
is documented in `../../idtx-forge/README.md`. These are automated host/target-build
checks; headset execution and editor GUI interaction require their device/UI lanes.
