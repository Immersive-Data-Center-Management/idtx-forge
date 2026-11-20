# IDTX Poser

![profile: authoring](https://img.shields.io/badge/profile-authoring-blue)

The **transform & variant authoring** app. A thin binary that launches the shared
[`idtx-studio-shell`](../../crates/idtx-studio-shell) with `ProductProfile::POSER`
(the shell's `authoring` feature). It edits a single stage — it is not a project tool.

## What it does

- **Transform authoring** — move/rotate/scale prims with the interactive gizmo or the
  inspector fields. Drags record a single undo step with the exact authored Euler values
  (so undo/redo round-trips the triple, not just the matrix).
- **Variant authoring** — switch variant-set selections.
- **Add / remove prims** — create new prims (e.g. `Xform`) and soft-delete prims.
- **Backend live-sync** *(with `remote`)* — connect to a collaboration server and
  co-edit a stage in real time: transform/create/set-active edits sync both ways over a
  WebSocket, and HTTP(S) USD assets stream through the resolver.
- Renders with soft shadows enabled (look-dev).

All edits land in the in-memory **session layer**; the root layer is written only on save.

## What it deliberately does **not** do

No projects, levels, behaviors, in-editor player transport (play/stop), or cooking —
those are [Conductor](../idtx-conductor/README.md) capabilities. "Static" here means the
Poser does not *simulate* (no behavior tick, no physics in the viewport), not that the
stage is immutable.

## Run

```bash
cargo run -p idtx-poser -- path/to/stage.usd
cargo run -p idtx-poser --features remote -- path/to/stage.usd   # enables Open Remote + live-sync
```

Requires `IDTX_USD_DIR` set (see the [workspace README](../../README.md#prerequisites)).
`remote` compiles the C++ HTTP resolver plugin (needs `IDTX_USD_DIR` at build time) and
pulls in the collaboration client; credentials come from the OS keychain +
`~/.idtx-studio/config.toml`.
