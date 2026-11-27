# Project and input: complete files

Save these files at the project root. Manifest paths are relative to that root. The built-in player is selected by leaving `player_stage` absent. The `[build]` values are persisted authoring settings; see [delivery targets](../delivery/targets.md) for which settings the current cook actually applies.

Keep the full input map when extending this example: the built-in player needs its movement actions, and the Pointer binding declares the literal `widgets.click` action. See [the project contract](../composition/project.md) and [interaction tutorial](../tutorial/04-interactions.md).

## `project.toml`

<!-- example-file: project.toml -->

```toml
# Paths are relative to this project root. Copy this directory before editing.
version = 1
name = "Signal Room"
default_stage = "levels/hub.usda"
start_level = "levels/hub.usda"
levels = ["levels/hub.usda", "levels/lab.usda"]
asset_roots = ["assets"]
# An omitted player_stage selects the built-in desktop/XR player.
# An omitted transfer_level selects the runtime's hard-cut transition.

[unit_scale]
meters_per_unit = 1.0

[build]
msaa = 4
target_fps = 90

[network]
allow = false

```

## `input_map.toml`

<!-- example-file: input_map.toml -->

```toml
# Actions consumed by the built-in desktop and XR rigs, plus widget clicks.
[actions.move_forward]
kind = "axis"
bindings = [{ source = "Key/W", scale = 1.0 }, { source = "Key/S", scale = -1.0 }]

[actions.move_right]
kind = "axis"
bindings = [{ source = "Key/D", scale = 1.0 }, { source = "Key/A", scale = -1.0 }]

[actions.look]
kind = "axis2"
bindings = ["Mouse/Delta"]

[actions.move]
kind = "axis2"
bindings = ["Controller/MetaQuest/LeftThumbstick"]
radial_deadzone = 0.15

[actions.turn]
kind = "axis"
bindings = ["Controller/MetaQuest/RightThumbstickX"]
radial_deadzone = 0.6

[actions.teleport]
kind = "button"
bindings = ["Controller/MetaQuest/RightThumbstickPress"]

[actions."widgets.click"]
kind = "button"
bindings = ["Mouse/Left", "Controller/MetaQuest/RightTrigger", "Controller/MetaQuest/A", "Gesture/pinch/right", "Gesture/pinch/left"]

```
