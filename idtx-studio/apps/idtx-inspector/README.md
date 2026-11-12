# IDTX Inspector

![profile: read-only](https://img.shields.io/badge/profile-read--only-blue)

The **read-only USD stage viewer**. A thin binary that launches the shared
[`idtx-studio-shell`](../../crates/idtx-studio-shell) with
`ProductProfile::INSPECTOR` — every authoring capability is switched off.

## What it does

- Opens a single USD stage directly (no project) and renders it in the `wgpu`
  viewport with an orbit camera.
- Inspects the stage: prim tree, type/kind, applied API schemas, visibility, payload
  state, variant sets + current selection, the authored local transform, and the layer
  stack / composition arcs.
- Loads and unloads payloads for inspection.

## What it deliberately does **not** do

No transform/variant editing, no add/remove prims, no projects, levels, behaviors,
player transport, or cooking. Soft shadows are off (a look-dev/authoring concern). If you
need to *change* the stage, use the [Poser](../idtx-poser/README.md) or
[Conductor](../idtx-conductor/README.md).

## Run

```bash
cargo run -p idtx-inspector -- path/to/stage.usd
```

Requires `IDTX_USD_DIR` set (see the [workspace README](../../README.md#prerequisites)).

## Features

| Feature | Effect |
|---|---|
| `remote` | "Open Remote…" — browse and open server-hosted USD stages over HTTP(S) (compiles the C++ resolver plugin; needs `IDTX_USD_DIR` at build time). |
| `metrics` | Frame-timing instrumentation. |

```bash
cargo run -p idtx-inspector --features remote -- https://host/stage.usd
```
