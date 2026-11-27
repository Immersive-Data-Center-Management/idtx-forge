# IDTX Runtime

![profile: player](https://img.shields.io/badge/profile-packaged%20player-yellow)

The **packaged player** — the shipped experience, with no editor chrome. It wraps
[`idtx-runtime-core`](../../crates/idtx-runtime-core): it opens a USD stage in a
`StudioSession`, ticks behaviors + animation + physics, renders, and (on XR builds)
drives an OpenXR / CompositorServices session. This is also the binary the Conductor
spawns for **Preview in Runtime**.

## Two artifacts from one crate

- **Desktop / PC-VR binary** (`src/main.rs`) → `idtx-runtime[.exe]`. The Windows/macOS
  bundlers co-locate it next to the Conductor.
- **Android / visionOS cdylib** (`src/lib.rs`) → `libidtx_runtime.so` /
  `libidtx_runtime.dylib`, produced only by the `xtask` cross-builds. On visionOS the
  cdylib is winit-free and drives a CompositorServices + Metal + ARKit render loop.

## Run (desktop)

```bash
cargo run -p idtx-runtime -- --stage path/to/stage.usd [--mode xr|desktop]
cargo run -p idtx-runtime -- --stage s.usd --headless --frames 120   # CI / tests
```

With no `--stage`, it falls back to a beside-binary `run.toml` `[experience]`. Both
`xr` and `desktop` modes run the *same* frame loop; `xr` additionally starts an OpenXR
session + stereo render with a desktop mirror. Useful flags: `--token`/`--token-file`
(remote auth), `--controller-device <id>`, `--asset-search-dir <dir>` (repeatable),
`--allow-network`/`--allow-host <pat>`/`--replace-network-policy`, `--screenshot <png>`.

Requires `IDTX_USD_DIR` set (see the [workspace README](../../README.md#prerequisites)).

## Features

| Feature | Effect |
|---|---|
| `authoring`, `physics` *(default)* | USD authoring surface + `UsdPhysicsScene` → Rapier rigid-body simulation. |
| `pcvr` | Real OpenXR on Windows (D3D12). |
| `quest` | OpenXR on Android/Quest (Vulkan) — set by `cargo xtask build-quest-runtime`. |
| `visionos-immersive` | Apple Vision Pro CompositorServices immersive renderer (visionOS cdylib). |
| `mock-xr` | Deterministic injected XR poses for headless smoke tests. |
| `remote` | HTTP(S) stage + asset loading with bearer-token auth. |
| `openexec` | Runtime OpenExec dataflow (REST / compute-node values → session layer). |
| `metrics` | Per-frame render performance metrics. |
| `gesture-recognition` | Hand-gesture recognition (pinch → grab, …). |

## Packaging

The player is never built by hand for standalone targets; it is cross-compiled and
staged by the `xtask` cross-builds, then wrapped by a `Packager`
(`DesktopFolderPackager` / `MacAppPackager` / `AndroidApkPackager` /
`VisionOsAppPackager`). See
[bundling](../../README.md#bundling-the-os-applications) and
[ARCHITECTURE.md §6](../../ARCHITECTURE.md#6-android-quest--apple-vision-pro).
