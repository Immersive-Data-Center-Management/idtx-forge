# Experience authoring contract for coding agents

Use this page as a working contract when assisting a developer who has a
packaged Conductor application, this book, and an experience directory.
No IDTX engine checkout is needed. The installed behavior SDK is the compile
authority; this book describes the public API and the effects an author can
expect. Start with [setup](../setup.md), then use the
[API task index](sdk-overview.md) and [complete SDK reference](behavior-sdk.md).

Before changing an experience, read its `project.toml`, `input_map.toml`,
referenced USD layers, and behavior source. Record the installed Conductor
and supplied target runtime versions. Keep authored files portable and keep
generated build/cache files separate. An agent with only filesystem access
can edit and check contracts; runtime observations still require Conductor
or its supplied player. Report the verification actually performed.

## Begin behavior edits in the Inspector

**Always start behavior editing from Conductor’s Inspector > Behavior panel:**
use **Create Behavior…** for a new attachment or **Edit Code** for an existing
one. These actions generate or refresh `.idtx/behavior-ide/behaviors.code-workspace`
and the Cargo wrappers that reference the original source, matching SDK, Rust
toolchain and WASM target. Use that workspace for rust-analyzer completion,
navigation, syntax/type diagnostics and checks on save. Opening a loose `.rs`
file or only the project folder does not establish this context.

Use **Set up rust-analyzer** if the Inspector reports the extension missing
from the selected VS Code profile. **Regenerate Editor Workspace** repairs
generated settings and source/SDK paths after changes. Edit authored sources;
let Conductor own generated manifests and wrappers. For inherited code, open
its owning asset to edit the shared implementation, or use **Make Local Copy…**
for a distinct implementation on that attachment. Follow the
[authoring walkthrough](../tutorial/03-behaviors.md#start-every-edit-from-the-inspector).

A filesystem-only agent should work on the authored files referenced by this
prepared workspace and report when Inspector setup or editor validation still
needs the developer. Save all entry and helper files before Inspector
**Cargo Check**, **Verify capabilities**, preview or cooking.

## Files and composition

An experience is a directory containing an authoring manifest, USD layers,
Rust behavior entries with optional helper modules, and referenced assets. Signal Room provides the
complete [worked example](../tutorial/01-example-tour.md). A typical tree is:

```text
my-experience/
  project.toml                 # Authoring project schema version 1
  input_map.toml               # Logical actions and physical bindings
  levels/
    hub.usda                  # Registered levels; defaultPrim = World
    lab.usda
  assets/
    exhibit.usda              # Reusable USD references
  behaviors/
    console.rs                # Behavior entry; may use helper modules
    pointer.rs
  extensions/
    specimen.usda             # Optional registered spawn prototype
  .idtx/                      # Generated local tooling/cache data
```

Paths in `project.toml` resolve relative to the project directory. USD asset
paths resolve relative to the layer authoring that opinion. For example, a
behavior source authored in `assets/exhibit.usda` uses
`@../behaviors/console.rs@`, even when a level references that asset.
See [project composition](../composition/project.md) and
[USD composition](../composition/usd.md) for overrides and registrations.

Use Y-up and metres in this book's projects: `upAxis = "Y"`,
`metersPerUnit = 1`, and an appropriate default prim. Layer
`customLayerData.idtx.stageKind` classifies `level`, `asset`, or `player`; it
does not register the file as a playable level. The project registry and
stage classification must agree. An omitted `player_stage` selects the
supplied desktop/XR player; an omitted `transfer_level` selects a hard cut.

Authoring `project.toml` uses version **1**. The cooked runtime manifest uses
version **2** and is produced by the cook pipeline. Guest behavior ABI is
**3**. These version numbers describe separate formats. Do not copy the
cooked manifest over `project.toml`, or hand-edit generated WASM/module paths
as a substitute for compilation. See [cooking](../delivery/cooking.md).

## A complete minimal behavior contract

The following files define an exhibit whose visibility toggles with `T`.
They illustrate a source attachment, typed configuration, action declaration,
physical binding, persistent state, and deferred USD edits. This small
contract is a starting point for behavior authoring; Signal Room adds a
visible room, widget controls, pointer routing, travel, and spawning.

Create `project.toml`:

```toml
version = 1
name = "Toggle Exhibit"
default_stage = "levels/hub.usda"
start_level = "levels/hub.usda"
levels = ["levels/hub.usda"]

[unit_scale]
meters_per_unit = 1.0

[build]
msaa = 4
target_fps = 90

[network]
allow = false
```

Create `input_map.toml`. Quote an action name containing a dot so TOML does
not interpret it as nested tables:

```toml
[actions."exhibit.toggle"]
kind = "button"
bindings = ["Key/T"]
```

Create `levels/hub.usda`:

```usda
#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
    metersPerUnit = 1
    customLayerData = {
        dictionary idtx = {
            string schemaVersion = "1"
            token stageKind = "level"
            string name = "Toggle Exhibit"
        }
    }
)

def Xform "World"
{
    def Cube "Exhibit" (prepend apiSchemas = ["IdtxBehaviorAPI"])
    {
        double size = 0.5
        double3 xformOp:translate = (0, 1, -2)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        token idtx:behavior:kind = "runtime"
        string idtx:behavior:name = "exhibit_toggle"
        string idtx:behavior:version = "0.1.0"
        string idtx:behavior:uuid = "de1845a1-e029-4936-b3ea-43810a54f28d"
        token idtx:behavior:scope = "entity"
        bool idtx:behavior:enabled = true
        asset idtx:behavior:source = @../behaviors/toggle.rs@
        uniform token[] idtx:behavior:capabilities = ["input", "stage_edit"]
        uniform token[] idtx:behavior:inputs = ["exhibit.toggle:button"]
        custom string idtx:behavior:context = '{"initial_visible":true}'
    }
}
```

Save the complete listing as `behaviors/toggle.rs` to reconstruct this example,
then open the attachment through **Edit Code** in the Inspector before editing.
For a new behavior of your own, start with **Create Behavior…** and adapt its
generated entry file:

```rust
use idtx_behavior_sdk::prelude::*;

#[derive(Deserialize)]
#[serde(crate = "idtx_behavior_sdk::serde")]
struct Settings {
    initial_visible: bool,
}

#[derive(Default)]
pub struct ExhibitToggle {
    action: Option<input::Action>,
    initial_visible: bool,
    visible: bool,
}

#[behavior(kind = "runtime")]
impl ExhibitToggle {
    fn on_start(&mut self, ctx: &Ctx) -> Result<()> {
        let mut scratch = [0u8; 128];
        let settings: Settings = ctx.context(&mut scratch)?;
        self.action = Some(input::action("exhibit.toggle"));
        self.initial_visible = settings.initial_visible;
        self.visible = settings.initial_visible;
        ctx.stage_edit().set_self_visibility(self.visible)
    }

    fn on_update(&mut self, ctx: &Ctx, _dt: f32) -> Result<()> {
        if self.action.is_some_and(|action| action.pressed()) {
            self.visible = !self.visible;
            ctx.stage_edit().set_self_visibility(self.visible)?;
        }
        Ok(())
    }

    fn on_stop(&mut self, ctx: &Ctx) -> Result<()> {
        ctx.stage_edit().set_self_visibility(self.initial_visible)
    }
}
```

Open this project in Conductor, select `Exhibit`, and use **Edit Code** in its
Behavior inspector to prepare the workspace. After saving edits, run
**Cargo Check** and **Verify capabilities**. The required grants
are `input` and `stage_edit`. During play, click the viewport to focus it and
press `T`; verify a visible cube becomes invisible and the next press restores
it. Stop should restore the configured initial visibility. This establishes
behavior effect in that session; the runtime preview and cooked target need
their own checks.

## USD attachment rules

`IdtxBehaviorAPI` is a single-apply schema. The following attributes form the
public attachment contract; add them to the owning prim or author stronger
opinions in an appropriate composition layer:

| Attribute | USD type | Meaning |
|---|---|---|
| `idtx:behavior:name` | `string` | Module/display name. |
| `idtx:behavior:version` | `string` | Behavior version string. |
| `idtx:behavior:uuid` | `string` | Stable module identity; use a fresh identity for a distinct module. |
| `idtx:behavior:kind` | `token` | `runtime` or `editor`; must agree with the source macro. |
| `idtx:behavior:scope` | `token` | `entity` by default, or `stage`. |
| `idtx:behavior:enabled` | `bool` | Defaults to true; false disables dispatch. |
| `idtx:behavior:serverAuthoritative` | `bool` | Defaults to false; additionally permits dispatch on a server-role pawn when the runtime supplies that ownership role. |
| `idtx:behavior:source` | `asset` | Author's `.rs` source, resolved from its authoring layer. |
| `idtx:behavior:module` | `asset` | Compiled `.wasm`, authored by compilation/cooking. |
| `idtx:behavior:capabilities` | `uniform token[]` | Exact granted import buckets. A stronger array opinion replaces the set. |
| `idtx:behavior:inputs` | `uniform token[]` | Entries `action_name:button`, `action_name:axis`, or `action_name:axis2`. |
| `idtx:behavior:context` | `custom string` | Immutable bounded JSON configuration snapshot. |
| `idtx:behavior:ship` | `bool` | Cook inclusion control for editor behaviors; runtime behaviors ship by kind. |
| `idtx:behavior:limits:memoryMb` | `int` | Optional clamp below the host's default memory limit. |

A shared module receives a separate WASM instance and separate Rust fields
for each live attachment. Sharing a UUID/source does not share behavior
instance state. Stage-scoped bindings receive a null `ctx.entity()`; resolve
a concrete prim before a scene operation requiring an owner. A source-only
attachment does not execute until its compiled module resolves.

Do not create a behavior `idtx.toml` sidecar or use `#[param]` declarations.
This authoring path stores the contract in USD and reads configuration through
`ctx.context()`. Parameter-related legacy attributes do not imply a public
parameter getter.

## Rust source and lifecycle

Conductor loads each author entry as a normal Rust module in a generated
standalone guest crate. Helper modules and source-relative include data are
supported within the source collection described in the
[behavior walkthrough](../tutorial/03-behaviors.md#organize-source-and-helper-modules).
Use `idtx_behavior_sdk::prelude::*`; the generated dependency
set contains the SDK and `arrayvec`. Use SDK Serde/glam reexports. Do not add
engine workspace crates, assume arbitrary additional dependencies, reference
private `__rt` plumbing, or add a panic handler.
Conductor supplies the crate-level `#![no_std]` wrapper. Keep crate-level
configuration out of the author module; normal module documentation (`//!`)
and module-level attributes are supported.

Keep one `Default` state type with one annotated inherent implementation.
`#[behavior]` defaults to runtime; `#[behavior(kind = "editor")]` selects the
other supported kind. No capability, parameter, or scheduling arguments are
accepted by this macro. Replace Conductor skeleton `todo!()` bodies before
play. Optional lifecycle methods use exactly these signatures:

```rust
// Signature list; these methods belong inside the annotated impl.
fn on_attach(&mut self, ctx: &Ctx) -> Result<()>;
fn on_start(&mut self, ctx: &Ctx) -> Result<()>;
fn on_input_capture(&mut self, ctx: &Ctx) -> Result<()>;
fn on_input(&mut self, ctx: &Ctx, action: InputActionId, phase: InputPhase) -> Result<()>;
fn on_update(&mut self, ctx: &Ctx, dt: f32) -> Result<()>;
fn on_late_update(&mut self, ctx: &Ctx, dt: f32) -> Result<()>;
fn on_editor_update(&mut self, ctx: &Ctx, dt: f32) -> Result<()>;
fn on_interact(&mut self, ctx: &Ctx) -> Result<()>;
fn on_event(&mut self, ctx: &Ctx) -> Result<()>;
fn on_stage_event(&mut self, ctx: &Ctx, event: StageEvent) -> Result<()>;
fn on_stop(&mut self, ctx: &Ctx) -> Result<()>;
fn on_detach(&mut self, ctx: &Ctx) -> Result<()>;
fn on_destroy(&mut self, ctx: &Ctx) -> Result<()>;
```

The signature list is a reference, not a compilable inherent implementation;
provide bodies for methods used by a behavior. `dt` is seconds;
`ctx.time_code()` is a stage time code. A retained instance may start again
after stop, so reset play-specific state in `on_start`. `on_interact` needs
application dispatch; declaring it does not establish pointer routing.
Instance and callback ordering is bounded and best effort under overload.
See [lifecycle details](behavior-sdk.md#lifecycle-signatures) and
[host scheduling](behavior-host.md#scheduling-and-errors).

## API groups and grants

Capability tokens are exact. Use the inspector's actual WASM import report
to determine required grants; unknown token spelling can leave an operation
ungranted. Creating an SDK grouping handle does not itself establish an
operation's runtime availability.

| Grant | Public entry points and use |
|---|---|
| `input` | `input::action`, per-source `Action` reads and `CaptureToken`; declared logical actions. |
| `scene_query` | `Entity::find`, hierarchy/visibility, composed relationships, curve samples and raycasts. |
| `xform_write` | `Entity::set_xform`; bridge transform writes. Transform reads also link with `scene_query`. |
| `stage_edit` | `ctx.stage_edit()` local transforms, visibility, bounded definitions/removal, HUDs/readouts, prepared reference spawning. |
| `events` | `events::post/broadcast`, `ctx.events().emit/broadcast`, `ctx.event()` during `on_event`. |
| `playback` | `ctx.animations().clip/clip_on`, transport/release/status and `playback::command`. |
| `widget` | Child/background callbacks, source-aware pointer handling and explicit activation. |
| `overlay` | `ctx.highlight()` and `ctx.draw()` temporary render feedback. |
| `xr_hmd` | `ctx.xr()` tracked poses, hands/joints, source snapshots and recenter. |
| `physics` | `ctx.physics().body/suspend/resume/set_simulated`; supplied runtime simulation required. |
| `level_control` | `ctx.level().goto/goto_with_spawn/cancel/read_state/set_state`. |
| `net` | `net::request_async/post_json_async`; runtime network support and project policy also required. |
| `auth_write` | `auth::*` credential write/revoke methods within configured authorized origins. |

`ctx.entity()`, `delta_seconds()`, `time_code()`, `camera()`,
`context::<T>()`, logging, and hook-result reporting are ambient.
`camera()` returns `Option<CameraView>`; device/camera availability must be
handled. For exact method arguments and return types follow the
[SDK reference](behavior-sdk.md), rather than inferring methods from these
group names. `Ctx` has no `self_entity`, general USD accessor, or
`scene_write` accessor.

## Scene and state reasoning

Use composed USD relationships for object bindings, and relative child/sibling
queries for fixed structure inside reusable assets.
`ctx.scene().child(parent, name)` returns `Result<Option<Entity>>`;
handle both a missing child and a query error. Preview may re-root level
content below `/World/Levels/<stem>`; cooked multi-level composition uses
`/World/level`. The persistent player is under `/World/Player`. Do not hard-code
an authored `/World/Beacon` path into a behavior that moves with its level.
See [player and levels](../composition/player-and-levels.md).

`Entity` is an opaque, generation-checked handle for the current scene.
Removal, reprojection, or travel can invalidate cached handles. Handle
`StaleHandle`, resolve again when appropriate, and store stable values rather
than handles across levels.

For USD-authored objects use `ctx.stage_edit().set_transform(entity, &pose)`.
`Entity::get_xform/set_xform` address bridge components that an ordinary
hydrated USD prim may not possess. To derive an authored object's local pose,
use `parent_world.inverse() * entity.world_matrix()?`, then
`Transform::from_matrix`. Keep the intended local pose in behavior state.
Accepted stage edits enqueue session-layer opinions and become observable
after the session reconciles; an immediate query in the same hook can read
the previous snapshot. There is no general SDK `set_attribute` operation.

`ctx.context::<T>(&mut scratch)` decodes a binding snapshot fixed during each
hook. A valid authored change reattaches at reconciliation, resets Rust fields
and reruns attachment/start hooks. Invalid authored JSON prevents attachment;
it is not replaced by empty/default configuration. Maximum JSON is 8192 bytes. Use
`#[serde(crate = "idtx_behavior_sdk::serde")]` on derives and
`ContextString<N>` for retained UTF-8 strings; `N` counts bytes. Validate
domain rules such as finite values and nonempty level IDs after decoding.
`ctx.scene().custom_data(entity, "idtx", scratch)` reads the supported `idtx`
snapshot. Nested selectors such as `idtx:ui:label` address dictionaries inside
that supported root; the generic key argument does not promise unrelated
custom-data roots. Pure `context::decode` can be tested natively.

## Composed object bindings and curves

Author an object link as a USD relationship, for example
`rel idtx:behavior:beacon = </World/Beacon>`, then query
`ctx.scene().relationship_targets(ctx.entity(), name, &mut output)`. USD
reference/payload composition and relationship forwarding resolve the target;
a string in JSON does not receive this remapping. Types such as
`scene::RelationshipTarget` are exported by the scene module, not the prelude.

The result exposes `revision`, `resolved`, `total`, `written` and `truncated()`.
Only the written prefix is updated. Keep unresolved slots in their original
order; target status is `Live`, `Invalid` or `Unloaded`, and `entity()` returns
an entity only for Live. Missing relationships return NotFound; an existing
empty relationship succeeds with total zero. A forwarding cycle may expose
partial targets with resolved=false. Check the contract before accepting a
single target. The caller buffer is capped at 4096 targets and names at 256
bytes. Read again when the revision or composition changes; cached handles
can become stale independently. The updated Signal Room Console demonstrates
this contract.

`ctx.scene().curve(entity, curve_index)` returns a generation-checked handle
for one USD BasisCurves batch. `length()` and input arc distances are always
normalized stage-world metres, including composed scale and unit conversion.
`sample_distance(distance, CurveSpace)` returns double-precision point/tangent
arrays; `sample_distances(&distances, space, &mut output)` allows up to 256
samples. `CurveSpace::Local` returns authored local coordinates, while
`StageWorldMetres` returns normalized metre/Y-up coordinates. Choose the frame
deliberately before converting to a local transform for StageEdit. Queries
clamp at endpoints; implement periodic wrapping explicitly. Unsupported
types/bases return TypeMismatch, stale entities return StaleHandle, and short
buffers/nonfinite distances fail. Native reads return NotFound.

## Pointer identity and modal input

Use `input::PointerSource::{Desktop, Left, Right}` to identify a pointer.
`Unspecified` means unknown/ambiguous; it is never a synonym for right hand.
`input::action(name).from_source(source)` reads independently detected edges
and values for that source. `Action::source()` reports the selected source or
the aggregate action's provenance. Keep action declarations and physical
bindings explicit. A source selector does not synthesize tracking or input.

Use `on_input_capture(&mut self, ctx: &Ctx) -> Result<()>` to request modal
ownership before normal input consumers run. Retain an
`input::CaptureToken` acquired with local player, target, explicit source,
priority and up to 32 resolved actions. Do not reacquire it every update or
drop it immediately. Check active() after arbitration; release/drop it when
closing. The opening action must be released before confirming. The host
also releases ownership on detach, target removal and tracking loss. The
SDK reference describes eligibility, ties and native behavior.

`ctx.widgets().handle(hit)` reports occlusion. For explicit identity use
`handle_from(hit, source, click)`; its result exposes separate occluded and
handled flags. `activate(panel, path, source, confirm)` performs validated
button activation on a confirmation edge. `WidgetClick::source()` preserves
the delivered identity. See the [widget authoring reference](widgets.md) for
radial menus, polygon clipping and panel-background interaction.

## Widgets, asynchronous work, and travel

Widget panels register `ctx.widget().on_click("Button", Self::clicked)`.
They can also register `on_panel_click(Self::background_clicked)` for
background/bubbled activation; `WidgetClick::is_panel_background()` and
`kind()` distinguish it from a child click.
The callback has type `fn(&mut B, &Ctx, &WidgetClick) -> Result<()>`. A separate
pointer behavior raycasts and calls `ctx.widgets().handle(hit)`;
the panel alone does not discover clicks. Use the
[interaction walkthrough](../tutorial/04-interactions.md) for exact pointer
code, `widgets.click` declaration, and desktop/XR bindings. The macro's
widget callback registry exists on WASM; when a source also supports native
tests, guard `on_click` registration with `#[cfg(target_arch = "wasm32")]`.

Spawn only prepared prototype IDs registered in `project.toml`, for example:

```toml
[runtime_prototypes.specimen]
asset = "extensions/specimen.usda"
prim = "/Specimen"
```

Use an ASCII identifier for the registered ID and a concrete parent entity.
`spawn_reference_async(parent, name, prototype_id, &pose, &context, handler)`
accepts a handler of type `fn(&mut B, &Ctx, SpawnResult) -> Result<()>`.
A returned ticket establishes queue acceptance. The terminal callback reports
`Ready` or `Failed`; `Ready` waits for projection and required behavior
initialization. Do not poll the same ticket while the callback owns it.
Handle synchronous rejection separately from terminal failure, and expect
pending work to be canceled if its requester stops or disappears. See
[spawn contracts](behavior-sdk.md#reference-spawning-and-callbacks).

Named events are deferred targeted emissions or broadcasts. Use
`events::post(live_target, name, bytes)` for validated explicit targeting, and
`events::broadcast(name, bytes)` for deliberate global delivery. Names are
nonempty and at most 256 bytes; payloads are at most 64 KiB. The existing
`ctx.events()` wrappers remain available, with different native-stub behavior.
Receivers read the current name/payload during `on_event`; getters return
true lengths, so check buffer bounds before decoding. The envelope has target,
name and bytes; it has no general sender-aware routing metadata.

Declare actor clips with USD `idtx:animation:contractVersion`, `idtx:animation:clipNames`
and per-name start/end/channel/targets/source opinions. Use `clip(label)` for
the caller or `clip_on(owner, label)` for another bound owner, then
`play/pause/stop/seek/set_speed/set_looping/release`. `playback::command` also
queues transport, but the current host rejects an empty label despite the SDK
comment about default playheads. Use a nonempty explicit name. Read `Clip::status` or
`read_status` on a later tick; inspect state, error, run_id and the last-command
outcome. `PlaybackStatus` is in the animations module, not the prelude.
`stop` rewinds while `release` restores containing-timeline ownership.

Named clips and anonymous discovered groups coexist. Lifecycle events include
animation.completed/interrupted/failed and are delivered to bindings
on the clip owner. Their JSON payload includes owner, clip, channel, run_id
and error. Delivery is bounded; status remains queryable if an event is dropped.
Do not carry its owner handle across level teardown. There is still no public
clip catalog enumeration, marker subscription or arbitrary attribute watch.
Use the [complete animation and routing exercise](../tutorial/06-animation-and-routing.md)
and [SDK reference](behavior-sdk.md) for exact declaration and status contracts.

`ctx.level().goto("lab")` uses a cooked level ID derived from a registered
level's file stem. A path such as `levels/lab.usda` is not the API target.
Travel requires the cooked registry and is deferred.
`goto_with_spawn("lab", "entrance")` selects a named spawn.
`read_state/set_state` carry bounded bytes during one running experience;
they do not write a save file. Keep entity handles out of this state.

HTTP is asynchronous: use `net::request_async` or `post_json_async` with
`fn(&mut B, &Ctx, HttpResponse) -> Result<()>`. Copy required response data
while the callback is live; check `read_body`'s true length and status zero
for transport failure. Blocking HTTP wrappers are rejected by the player.
The binding's `net` grant, project policy, and supplied runtime network support
must all allow the request. HTTP and spawn dispatchers currently discard
callback return errors; handle and log failures inside those callbacks.
Never put credentials in USD context or log them.

## Verification and evidence

Use the [testing chapter](../testing.md) and its acceptance criteria. Each
step answers a different question:

1. Check manifest paths, USD references, JSON context types, grants, and input
   declarations against authored files.
2. Run native tests for pure calculations/decoders when the change warrants
   them. SDK native stubs provide no scene/input/physics simulation and do
   not deliver real asynchronous callbacks.
3. In packaged Conductor, run **Cargo Check**, then **Verify capabilities**
   for changed behaviors. The latter builds actual WASM and compares its
   imports with authored grants without executing the guest.
4. Exercise the authoring transport for attachment, hooks, routing,
   session edits, and error reporting.
5. Launch the supplied Runtime preview to exercise the composed player,
   runtime physics, XR/backend behavior, and packaged runtime features.
6. Cook with the appropriate supplied target runtime, then test the delivered
   package on its target device. Test multi-level travel against the cooked
   registry.

An agent should give the developer changed files, the expected observable
effect, performed checks and their results, and remaining runtime/device
checks. Successful compilation/import validation cannot establish a scene
effect, a prepared spawn's readiness, clip sampling, network delivery, or
device performance. When a package lacks an API/runtime feature, obtain a
matching package or implement with its supported public surface.
