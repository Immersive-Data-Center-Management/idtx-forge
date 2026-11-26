# Find a behavior SDK API

The [public SDK reference](behavior-sdk.md) documents the packaged behavior APIs with complete examples. The [host contracts](behavior-host.md) explain attachment, capabilities, execution limits and deferred work. Use this page to reach the section you need. Coding agents should also read the [experience authoring contract](agent-guide.md).

| Task | Reference | Required grant / key constraint |
|---|---|---|
| Create a behavior and choose hooks | [Start a behavior](behavior-sdk.md#start-a-behavior), [lifecycle](behavior-sdk.md#lifecycle-signatures) | One annotated implementation per module; runtime/editor kind must match binding |
| Access the owning entity and current frame | [Ctx](behavior-sdk.md#ctx-entry-points) | Stage-scoped bindings have a null owning entity |
| Read/write transforms or visibility | [Entities and transforms](behavior-sdk.md#entities-and-transforms) | `scene_query`/`xform_write` for reads; use `stage_edit` for hydrated USD transform edits |
| Decode authored configuration | [Typed context](behavior-sdk.md#typed-contexts-and-custom-data) | Immutable bounded JSON snapshot; own strings retained after a hook |
| Read actions and handle edges | [Input](behavior-sdk.md#input) | `input`; declare actions on the attachment and define physical bindings in `input_map.toml` |
| Separate controller inputs or capture a modal menu | [Pointer sources and capture](behavior-sdk.md#pointer-sources-and-modal-input-capture) | `input`; explicit source; retained token acquired in `on_input_capture`; wait for neutral |
| Find children, cast rays and inspect data | [Scene queries](behavior-sdk.md#scene-queries) | `scene_query`; distinguish a miss from invalid handles |
| Bind to objects inside referenced assets | [Composed relationship targets](behavior-sdk.md#composed-relationship-targets) | `scene_query`; inspect completeness, status and revision instead of storing occurrence paths in JSON |
| Follow or sample authored USD paths | [Native BasisCurves by distance](behavior-sdk.md#native-basiscurves-by-distance) | `scene_query`; explicit batch index; distances in world metres; linear/cubic Bézier support |
| Author runtime USD opinions | [Session edits](behavior-sdk.md#session-edits) | `stage_edit`; edits reconcile at the session boundary |
| Spawn a registered USD reference | [Spawn callbacks](behavior-sdk.md#reference-spawning-and-callbacks) | `stage_edit`; registered prototype ID; wait for Ready |
| Route panel input and receive button/background clicks | [Widgets](behavior-sdk.md#widgets), [widget layouts](widgets.md) | `widget`; WASM callback registry; handling and occlusion are distinct |
| Navigate/highlight widgets without a ray | [Direct navigation](behavior-sdk.md#panel-backgrounds-pointer-identity-and-direct-navigation) | `widget`; supply explicit source and confirmation action; `input` for reading actions |
| Read HMD/controller/hand poses | [XR](behavior-sdk.md#xr-poses-and-hand-tracking) | `xr_hmd`; test tracking/backend availability on the device |
| Suspend/resume a tracked body | [Physics](behavior-sdk.md#runtime-physics-control) | `physics`; runtime simulation and tracked body required |
| Draw temporary outlines and aim cues | [Feedback](behavior-sdk.md#transient-visual-feedback) | `overlay`; reissue per-frame feedback while needed |
| Emit/receive named events | [Events and animation](behavior-sdk.md#named-events-and-animation) | `events`; use `post` for a live target or explicit `broadcast`; deferred routing |
| Author reusable named animation actions | [Named clips and ownership](behavior-sdk.md#authored-named-clips-and-pose-ownership) | `playback`; versioned USD declaration; exclusive target ownership; release to resume timeline sampling |
| Read animation status or handle completion | [Status and lifecycle events](behavior-sdk.md#status-and-animation-lifecycle-events) | `playback` for status, `events` for notices; compare run IDs; queue success is not application success |
| Travel or carry state across levels | [Level control](behavior-sdk.md#level-travel-and-carried-state) | `level_control`; cooked registry; instance handles do not cross levels |
| Fetch HTTP data or use credentials | [Async HTTP and auth](behavior-sdk.md#asynchronous-http-and-authentication) | `net`/`auth_write`, runtime features and project/origin policy |
| Interpret errors and test pure logic | [Errors and native tests](behavior-sdk.md#errors-logging-and-native-tests) | Native wrappers are inert and cannot validate scene effects |

For Signal Room, begin with [the behavior chapter](../tutorial/03-behaviors.md).
For an optional symbol index, follow the [bundled-SDK Rustdoc instructions](behavior-sdk.md#errors-logging-and-native-tests).
The normal verification path uses Conductor's **Cargo Check** and
**Verify capabilities** controls, followed by runtime acceptance. It needs
the packaged app and its SDK; it does not need an engine checkout.
