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
