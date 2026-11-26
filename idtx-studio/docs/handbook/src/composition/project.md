# Create and organize a Conductor project

A Conductor project is a folder with a `project.toml` at its root. USD files describe the world; the manifest registers its levels, selects the player and controls cooking. You own this project folder. It can live anywhere writable, independently of the Conductor installation.

For this book, copy the supplied `examples/signal-room` folder to `<projects>/signal-room`. The [complete project files](../example/project-files.md), [USD files](../example/usd-files.md) and [behavior files](../example/behavior-files.md) also let you reconstruct the project from the book. Finish [installation and compiler setup](../setup.md) before opening it.

Open the installed application and choose **Project > Open Project…**, then select `signal-room/project.toml` in the file picker. The startup dialog also offers **Open Project…**. On Windows, the equivalent command is:

```powershell
& "C:/IDTX/Conductor/idtx-conductor.exe" --project "C:/IDTX/Projects/signal-room/project.toml"
```

On macOS, launch Conductor normally or run the executable inside its installed app:

```bash
"/Applications/IDTX Conductor.app/Contents/MacOS/idtx-conductor" \
  --project "$HOME/IDTX/Projects/signal-room/project.toml"
```

Adjust the installation and project paths to match your machine. Commands elsewhere in the walkthrough run from your **project root** unless stated otherwise.

## Separate the project from its stages

For a new experience, choose **Project > New Project…**. **File > New…** creates a USD stage, and **Project > Settings…** edits project configuration. Keep authored stages and their dependencies within the project root.

| File | What it owns |
|---|---|
| `project.toml` | Name, level registry, startup level, player stage, shared layers, network policy and build configuration. |
| `levels/*.usda` | Level composition, geometry, lighting and behavior attachments. |
| `assets/` | Reusable USD assets, textures and other referenced content. |
| `behaviors/*.rs` | Single-file Rust behavior source compiled by Conductor against its supplied SDK. |
| `input_map.toml` | Physical keyboard, mouse, controller and gesture bindings for logical actions. |
| `xr_controllers.toml` | Optional device-to-controller-mesh mapping. |
| `extensions/` | Optional additions to this book's example. |
| `build/` | Generated cook payloads and application packages. |

Generated WASM and build output can be recreated. Preserve your source files and manifest in version control or a backup.

## Read the manifest

The sample registers two levels and uses the bundled player. These are the core fields; the [complete sample manifest](../example/project-files.md) supplies the exact file. Put top-level fields before the first TOML table.

```toml
version = 1
name = "Signal Room"
default_stage = "levels/hub.usda"
start_level = "levels/hub.usda"
levels = ["levels/hub.usda", "levels/lab.usda"]
asset_roots = ["assets"]

[unit_scale]
meters_per_unit = 1.0

[build]
extra_features = []
loose_levels = false

[build.android]
texture_quality = "authoring"

[build.visionos]
texture_quality = "authoring"
```

The project manifest schema is version **1**. Unknown newer schema versions are rejected. An empty name falls back to the project folder name. Source paths are project-relative; referenced assets inside USD remain relative to their authoring layer.

| Top-level field | Meaning and practical choice |
|---|---|
| `default_stage` | Stage opened by the editor; also the cook's entry fallback. Use a registered world level. |
| `start_level` | Boot level for a multi-level package. Resolution falls back to `default_stage`, then the first registered level. Set it explicitly to a registry entry. |
| `levels` | Ordered registry of level stage paths. Two or more registered levels activate separate per-level cooked entries. |
| `transfer_level` | Optional lightweight stage shown while another level loads. |
| `asset_roots` | Additional resolver search roots inside the project. The project root is already implicit. |
| `player_stage` | Optional project-local player stage. Omit to use the player supplied with Conductor. |
| `assembly_stage` | Optional authored assembly for inspection/editing. Cooking generates its own assembly. |
| `shared_sublayers` | Global USD layers composed into every per-level assembly. |
| `runtime_prototypes` | Template registrations for runtime spawning; see [the spawn walkthrough](../tutorial/05-spawning.md). |

`[unit_scale].meters_per_unit` does not rewrite geometry. Author matching USD `metersPerUnit` and `upAxis` metadata. Use metres and Y-up throughout the first project.

## Manage the level registry

Open **Project > Settings… > Levels**. Use **New Level…** to create a stage or **Add Existing…** to register one you already authored. **Open Selected Level** or a double-click opens the selected level. Row controls reorder the list or remove an entry from the project; removal keeps the file. Missing source files are marked missing.

Set **Start level** and, if needed, **Transfer level** here. Registering levels enables packaging and transition lookup. Your behavior decides when to change levels.

Give level files distinct, simple names such as `hub.usda` and `lab.usda`. Cooked IDs come from their file stems and are case-sensitive: this example requests `hub` and `lab`. Invalid USD-name characters are sanitized and duplicate stems receive suffixes. Inspect the generated `run.toml` after cooking to confirm the final IDs. Renaming a source level can require updating behavior configuration.

## Import reusable assets

**Project > Import…** (`Ctrl+O`) brings external USD content into `assets/`:

1. Content already under `assets/` opens in place.
2. An external `.usdz` is copied into the project.
3. An external loose `.usd`, `.usdc` or `.usda` is packaged with its dependencies into a `.usdz`, then copied into the project.

The importer chooses an unused name and reports dependency problems. To retain an editable loose hierarchy, copy the asset and dependencies into the project yourself, then reference them from a level. Export other DCC formats to USD before importing.

## Configure optional capabilities

Signal Room is offline. A networked experience needs both the behavior's `net` capability grant and the project-wide network policy. They are separate settings. For example:

```toml
[network]
allow = true
allowed_hosts = ["api.example.com"]
max_response_mb = 1
max_asset_mb = 64

[behavior.limits]
queue_bytes = 2097152
http_response_bytes = 1048576
http_inflight = 8
```

`private_hosts` explicitly permits designated private-network hosts. `auth_origins` grants credential sharing to specified origins, including scheme and effective port. An allowed host alone does not grant credentials. Zero limit overrides select runtime defaults: 2 MiB deferred work, 1 MiB HTTP response reservation and 8 concurrent asynchronous requests per behavior. See [host capability and network contracts](../reference/behavior-host.md).

Build settings used by the current cook include `extra_features`, `loose_levels`, device texture profiles, APK assembly SDK/JDK roots and visionOS signing fields. **`msaa`, `target_fps`, `player_kind` and `player_locomotion` are stored but are not carried into generated `run.toml` by the current cook.** Configure the player through its authored stage/behaviors and verify the actual runtime result. `extra_features` declares required runtime support; the packaged application cannot add features to an already supplied native runtime.

## Checkpoint

Confirm the project opens on Hub, both levels appear in Settings, every dependency resolves and the manifest uses project-relative source paths. Save both the stage and project before previewing or building. Continue with [USD authoring](usd.md).
