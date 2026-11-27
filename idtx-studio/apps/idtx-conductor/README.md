# IDTX Conductor

Follow the [Experience Developer Handbook](../../docs/handbook/README.md) using
the packaged app. It includes every example file, SDK and coding-agent contracts,
USD authoring, composition, testing and device delivery without an engine checkout.

![profile: conductor](https://img.shields.io/badge/profile-project%20authoring%20%2B%20cook-blueviolet)

The **project-level authoring and orchestration** app — the only product with projects,
levels, behaviors, in-editor playback, and the cook/build/deploy pipeline. A thin binary
that launches the shared [`idtx-studio-shell`](../../crates/idtx-studio-shell) with
`ProductProfile::CONDUCTOR` and the `cooking` feature (on by default).

## What it does

Everything the [Poser](../idtx-poser/README.md) does (transform/variant authoring,
add/remove prims, backend live-sync, soft shadows), **plus**:

- **Projects** — create / open / discover an `idtx-project` (`project.toml`) with a
  root-scoped asset browser.
- **Levels** — author the ordered level-stage registry that layers onto a project.
- **Behaviors** — attach and configure sandboxed WASM behavior modules on prims; the
  single-file `.rs` sources are compiled to `.wasm` by the embedded behavior compiler.
- **Player transport** — play / stop the in-editor behavior simulation.
- **Preview in Runtime** — launch the active stage in the out-of-process `idtx-runtime`
  player exactly as a packaged experience would run (XR or flat desktop). The editor
  synthesizes a thin assembly layer (player prefab over the untouched world); the runtime
  self-probes for an OpenXR device and falls back to desktop if none is present.
- **Build & deploy** — cook the project into a stamped, reproducible payload and package
  it for Desktop, PCVR, Quest (`.apk`, assembled by `xtask`/CI), or Vision Pro (`.app`,
  with on-device deploy via `devicectl`). Builds run off the UI thread.

## Run

```bash
cargo run -p idtx-conductor -- path/to/project.toml     # open a project
cargo run -p idtx-conductor -- --project path/to/project.toml
cargo run -p idtx-conductor                             # startup project dialog
```

Requires `IDTX_USD_DIR` set (see the [workspace README](../../README.md#prerequisites)).
Standalone (Quest / Vision Pro) builds additionally need the cross-compiled runtime
staged beside the editor — see [bundling](../../README.md#bundling-the-os-applications).

## Features

| Feature | Effect |
|---|---|
| `cooking` *(default)* | The full cook surface: assembly composition, Preview + Build menus, the behavior compiler, cook-side OpenExec. Implies `build-tools` ⟹ `authoring`. |
| `build-tools` | Just the cook pipeline + Build menu (no preview surface). Superseded by `cooking`. |
| `remote` | Remote USD assets + arming the cook's resolver-packaging mode (keeps remote refs external). |
| `pcvr` | Real OpenXR device detection at preview-launch (to select the controller mesh). |

The "Build Vision OS → Build and deploy" item is enabled only when a paired Vision Pro is
detected; the Conductor installs a `tracing` subscriber so the cook/build/deploy
diagnostics reach the console.
