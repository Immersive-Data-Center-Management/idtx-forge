# F4 — USD-authored independent animation playback

**Status: approved and implemented.**

Usage and runnable authoring assets: [animation-playback example](examples/animation-playback/README.md).

Implementation entry points:

- [Native declaration reader and isolated USD bindings](crates/idtx-usd-sys/cxx/animation_playback.inc)
  and [safe stage API](crates/idtx-usd/src/animation.rs).
- [Reconciliation and exclusive transport](crates/idtx-scene/src/playback.rs),
  [session integration](crates/idtx-studio-session/src/animation_playback.rs),
  [SDK](crates/idtx-behavior-sdk/src/animations.rs) and
  [Conductor inspector](crates/idtx-editor-ui/src/inspector/panels/animation.rs).
- Integration coverage in `idtx-usd`, `idtx-scene` and `idtx-studio-session`
  `tests/animation_playback.rs`: native sources and time mapping, independent
  ownership, attached WASM completion feedback, undo, removal and USDZ packaging.

Native source overrides use private session compositions sharing the original
root/session layers, with action-local UsdSkel caches. This keeps transient
selection out of the main stage's persistence and collaboration paths. World
transforms combine the clip's local sample with the containing clock's placement;
native visibility samples retain explicit user/behavior overrides. Eligible
steady-state samples use joint-palette or transform-only projection.

The sections below retain the approved design and its pre-implementation evidence.

Verified on macOS with OpenUSD 26.11.1:

- 10 native/scene/session playback integration tests, including the headless
  GPU + authored WASM behavior case, passed.
- 95 scene unit tests, 53 SDK/ABI unit tests and 3 existing session transport
  regression tests passed.
- `cargo check -p idtx-studio-shell --features cooking` and
  `cargo check -p idtx-behavior-sdk --target wasm32-unknown-unknown` passed.
- `git diff --check` and local documentation links passed.

**Standards recheck (2026-11-23): recommend a schema-free authored property
contract instead of a new `IdtxAnimationClipAPI`.** OpenUSD supplies the native
animation, binding, composition and extensible storage mechanisms. IDTX still
supplies interactive transport policy. Use native `timecode` values for clip
endpoints instead of the previously proposed `double` values.

Reviewed against `d08dc59`, including animation enhancement `ea645ee`
(2026-11-23). The feature goal remains valid, but the original gap description
and implementation scope are partially superseded. The next increment should
extend the new discovery/hydration and transport paths with authored actions,
native source selection and observable playback outcomes.

## 1. What changed, and what remains

Paths below are relative to `idtx-studio`; line references describe this review's
baseline. These findings are from source inspection, not a runtime acceptance run.

| Area | Current implementation | Remaining F4 work |
| --- | --- | --- |
| Discovery | [`animation_catalog.rs`](crates/idtx-scene/src/animation_catalog.rs#L193) builds anonymous entries per animated prim, with composed ranges, kind, scope, optional name/tags and affected prim IDs. | Read authored named declarations, resolve actual consumers and distinguish discovery entries from runtime action bindings. Name/tags are currently placeholders. |
| Hydration | [`hydrate_following_playheads`](crates/idtx-scene/src/animation.rs#L837) creates players in production. The session invokes it on [open](crates/idtx-studio-session/src/lib.rs#L1423) and [topology refresh](crates/idtx-studio-session/src/lib.rs#L6123). | Authored policy, value-change reconciliation and removal. Existing matching heads preserve transport state, but every refresh marks them dirty and absent entries are not pruned. |
| SDK | [`Ctx::animations()` / `Clip`](crates/idtx-behavior-sdk/src/animations.rs) already expose play, pause, stop, seek, speed and looping. | Extend this surface with status, release-to-timeline and outcomes; validate numeric arguments. Existing seek narrows `f64` to the import's `f32`. |
| Ownership | [`ClipPlayhead::independent`](crates/idtx-scene/src/animation.rs#L302) distinguishes following from override. [Global sampling](crates/idtx-studio-session/src/lib.rs#L7506) excludes exact target IDs. | Account for descendants, ancestor projection roots, native skeletal consumers and behavior writes. Exact-ID exclusion does not bound expanded projection writes. |
| Transport | [`apply_playback_commands`](crates/idtx-studio-session/src/lib.rs#L9857) drives matching heads. | Missing targets/clips still silently disappear. Every command acquires ownership, including configuration changes. There is no channel arbitration. |
| Source selection | [`ClipSampleRequest`](crates/idtx-scene/src/animation.rs#L485) contains only time and targets. [`tick_clip_animation`](crates/idtx-studio-session/src/lib.rs#L7642) samples the existing composed binding. | Carry a resolved native source/binding. Changing the clip label cannot switch `skel:animationSource`. |
| Native caches | The [OpenUSD projection](crates/idtx-usd-sys/cxx/usd_shim.cpp#L4531) already resolves skeleton/source/mesh dependencies and validates native queries. [Dirty-cache handling](crates/idtx-usd-sys/cxx/usd_shim.cpp#L5892) exists. | Actor-scoped binding changes and invalidation. Current cache refresh clears and traverses the stage; [projection root expansion](crates/idtx-usd-sys/cxx/usd_shim.cpp#L5109) can include shared-source consumers beyond requested targets. |
| Events | [`events.rs`](crates/idtx-behavior-sdk/src/events.rs) and the host provide bounded, deferred named-event delivery. | Generate playback completion/interruption/failure events and expose durable queryable status. Generic event transport alone does not provide playback events. |
| Authoring | Undoable edits, [custom property creation](crates/idtx-usd-sys/cxx/usd_shim.cpp#L9477), [skeleton inspection](crates/idtx-editor-ui/src/inspector/panels/skeleton.rs) and [global transport](crates/idtx-studio-shell/src/render.rs#L2752) exist. | Add clip inspection, actor preview and typed `timecode` read/write support. Explicitly preserve custom/uniform property declarations in authoring. |

One scope clarification is needed: the native projection currently sends
blendshape-bound meshes to an explicit [rest-pose fallback](crates/idtx-usd-sys/cxx/usd_shim.cpp#L4828).
F4 must preserve those bindings and their diagnostics; working blendshape
deformation would require additional renderer work beyond this playback plan.
Existing skinning methods, joint remapping and supported CPU fallbacks remain
part of the source-switch regression coverage.

## 2. OpenUSD standards assessment and revised authored contract

### What the standard already supplies

Checked the published OpenUSD documentation and upstream `v26.08` interfaces.
The core facilities below provide useful building blocks, but no general-purpose
schema in those interfaces specifies the complete F4 action-controller contract.

| Standard facility | Appropriate use for F4 | Boundary |
| --- | --- | --- |
| `UsdSkelAnimation`, `UsdSkelBindingAPI` and `skel:animationSource` | Store native joint/blendshape animation and select the source bound to each actor's skeleton. | No named-action registry, independent clock, exclusive channel or completion event. |
| Native time-sampled Xform/visibility attributes | Store and evaluate rigid animation. | Sampling time and playback policy belong to the consuming application. |
| `UsdClipsAPI`: named clip sets, `assetPaths`, `active`, `times`, `primPath`, manifest | Standard sequencing and retiming of attribute values. Explicit time mappings support repetition, reverse and holds, including different schedules on different actors. | These are value-resolution schedules, not play/pause/event semantics. Clip layers do not supply relationships, defaults or composition arcs; they cannot time-switch `skel:animationSource`. |
| Variant sets | Package alternative native bindings/animation opinions and choose one per editable occurrence. | Selection is composition metadata, not time-sampled transport. Switching recomposes; ordinary stage flattening retains the selected result rather than an interactive library of alternatives. |
| `UsdCollectionAPI` | Standard target membership, subtree expansion and exclusions, if complex reusable target sets are needed. | Membership does not imply animation ownership or playback policy. F4's bounded explicit roots can use ordinary relationships directly. |
| `timecode` attribute type | Represent a time-valued endpoint whose value receives composition time offsets/scales. | It transforms according to the endpoint's own authoring/composition context, not an arbitrary relationship target's source clock. |
| Custom typed attributes/relationships; `customData` | Standard, plugin-free storage and composition of application data. | Custom names and their playback meanings remain an IDTX convention; other tools can retain/inspect them without implementing that convention. |
| `UsdMediaSpatialAudio`, `UsdGeomMotionAPI`, `TsSpline` looping | Audio scheduling; motion-blur computation; curve-level repetition, respectively. | None is a generic skeletal/rigid action controller. Audio's `timecode` endpoints are a useful precedent, but its playback properties are specific to audio. |

In particular, the earlier distinction about value clips should not be read as
"USD cannot loop or retime animation." It can. A runtime could also implement
commands by editing actor-local clip schedules. It would still need IDTX action
state, command semantics and events, and would introduce repeated metadata edits
and value-cache invalidation. Keep existing value-clipped data on the native
evaluation path; prefer the existing independent clocks for interactive playback.

### Recommended representation: custom properties, no new API schema

Author typed, namespaced custom properties on an editable actor `Xform` or
`SkelRoot`. This is supported by ordinary `UsdPrim::CreateAttribute` and
`CreateRelationship`; it needs no schema generation, plugin registration or
`apiSchemas` entry. Automatic hydration and a dedicated Conductor panel can use
an explicit version/clip roster instead of `HasAPI`.

Declare `custom uniform int idtx:animation:contractVersion = 1` and
`custom uniform token[] idtx:animation:clipNames`. Only listed clips participate;
names must be unique, nonempty single identifiers. The roster is a regular
strongest-value-wins attribute, not a listOp: an occurrence overrides the whole
roster to remove an inherited clip. An empty roster disables all declarations
on that owner without exposing weaker property opinions accidentally.

IDTX's shared reader/validator supplies defaults and detects malformed properties;
generic OpenUSD readers do not acquire schema fallbacks. Conductor authors
explicit policy defaults for discoverability. Runtime policy attributes must be
uniform and carry no time samples. The catalog's comment mentioning a future
`IdtxAnimationAPI` should describe this property convention instead.

Skeletal data remains native `UsdSkelRoot` / `UsdSkelSkeleton` /
`UsdSkelAnimation` / `UsdSkelBindingAPI`; rigid animation remains native
time-sampled attributes. The custom `source` below is an action's candidate
source. The effective skeletal binding is still the standard
`skel:animationSource` relationship on the actor's binding site.

| Property under `idtx:animation:<name>:` | Proposed rule |
| --- | --- |
| `targets` (custom relationship) | Required, nonempty, bounded roots within the owner's actor subtree. Normalize redundant descendant roots. Resolve consumers and reject a binding that would write outside that bound. |
| `source` (custom relationship) | Zero or one composed `SkelAnimation` prim. Absent means sample already-composed attribute/native binding animation over the range. External source prims may be shared read-only; evaluated actor consumers must remain bounded. |
| `start`, `end` (custom uniform timecode) | Explicit, finite endpoints, resolved by USD into composed-stage time codes, with `end > start`. |
| `speed` (custom uniform double) | Finite multiplier, default `1`; negative supports reverse, zero holds without completing. |
| `loop` (custom uniform bool) | Default `false`. |
| `autoPlay` (custom uniform bool) | Default `false`; start once when the declaration becomes active in a running experience. |
| `channel` (custom uniform token) | Default `body`; one active action per owner/channel. Overlapping writes across different channels or owners are diagnosed rather than resolved by iteration order. |

Use real relationship targets in the asset's namespace so USD references remap
them. Persist identity as composed owner path plus clip name; resolve
generation-scoped `PrimId`/entity handles for runtime use. Keep anonymous catalog
labels addressable for compatibility, while authored names belong to the playback
owner, which need not itself carry time samples.

Illustrative authoring overlay, assuming `/Actor/Rig` and its native animation
sources already exist in the asset. These properties are a proposed IDTX
convention, not a currently supported asset or standard playback schema:

```usda
#usda 1.0
over "Actor"
{
    custom uniform int idtx:animation:contractVersion = 1
    custom uniform token[] idtx:animation:clipNames = ["idle", "reaction"]

    custom rel idtx:animation:idle:targets = </Actor/Rig>
    custom rel idtx:animation:idle:source = </Actor/Rig/Animations/Idle>
    custom uniform timecode idtx:animation:idle:start = 0
    custom uniform timecode idtx:animation:idle:end = 48
    custom uniform double idtx:animation:idle:speed = 1
    custom uniform bool idtx:animation:idle:loop = true
    custom uniform bool idtx:animation:idle:autoPlay = false
    custom uniform token idtx:animation:idle:channel = "body"

    custom rel idtx:animation:reaction:targets = </Actor/Rig>
    custom rel idtx:animation:reaction:source = </Actor/Rig/Animations/Reaction>
    custom uniform timecode idtx:animation:reaction:start = 60
    custom uniform timecode idtx:animation:reaction:end = 84
    custom uniform double idtx:animation:reaction:speed = 1
    custom uniform bool idtx:animation:reaction:loop = false
    custom uniform bool idtx:animation:reaction:autoPlay = false
    custom uniform token idtx:animation:reaction:channel = "body"
}
```

### Other schema-free options considered

- **Existing behavior context + relationships:** feasible for a reusable
  initializer or a helper invoked by the gameplay behavior. The SDK already has
  [`Ctx::context`](crates/idtx-behavior-sdk/src/ctx.rs#L107) and
  [composed relationship reads](crates/idtx-behavior-sdk/src/scene/relationships.rs).
  It still needs a supported create/register-playhead host operation. JSON policy
  is harder to edit/override per field; JSON endpoint numbers do not receive USD
  time mapping. Context edits currently reattach/reset the behavior, requiring
  special reconciliation to preserve playback. A separate initializer must also
  account for the existing single behavior attachment per prim. This fits
  behavior-specific setup, but typed declarations better serve automatic
  hydration, Conductor inspection and state-preserving edits.
- **`customData` dictionary + relationships:** legitimate plugin-free metadata;
  dictionary keys compose independently. Keep object bindings as relationships,
  since strings containing prim paths do not remap through references. Typed
  properties make range types, property edits, blocking and inspection more
  explicit for this contract.
- **Native variants/value clips + behavior-only policy:** feasible when a pipeline
  already uses those representations. Requires a mapping from action names to
  those native choices plus independent runtime state. Alternate variants/clip
  sets must survive cook or be materialized before flattening; the current
  flattened-stage path cannot be assumed to preserve a selectable action library.

A codeless API remains a possible later formalization of a stable convention,
with schema discovery and defaults as its benefits. It is not required for
correctness or automatic hydration, and removing it does not make the remaining
custom playback meanings part of the OpenUSD standard.

### Timing and activation

- Use the composed stage's `timeCodesPerSecond`. Preserve native composition
  timing for animation samples.
- Author `timecode` endpoints in their layer's time domain and read their composed
  values through USD. Their values receive composition time offsets/scales,
  unlike plain `double` or JSON numbers. This corrects the original plan's need
  for manual occurrence overrides for ordinary reference retiming. For example,
  endpoints `0..48` through an offset of `100` and scale of `2` resolve to
  `100..196` when rates otherwise match.
- Source assets may enter through different arcs/rates from the declaration.
  Binding/import must obtain the source's effective stage-domain sample range
  and author endpoints in the selected edit target's domain using USD's time
  mapping. Do not copy raw source frame numbers, apply the composed offset twice
  or assume that a relationship target determines a `timecode` value's mapping.
  Report the effective range and validate it again during cook. Endpoint mapping
  does not automatically change the dimensionless runtime speed multiplier.
- Opening Conductor for inspection does not activate autoplay. Starting an
  experience, or attaching/loading an actor into an already running experience,
  activates it once. Unrelated hydration never restarts it. Explicit actor preview
  uses the same activation/controller path.
- Multiple autoplay declarations in one channel are a validation error. Multiple
  non-autoplay declarations in one channel are the expected idle/reaction case.

## 3. Runtime design

### A. Extend the catalog and reconcile bindings

Add authored declaration and resolved-binding records alongside anonymous
discovery. Keep catalog queries read-only; store playhead state separately in
`AnimClipPlayer`. Hydrate declarations even when the owner has no animated
attributes and even when there is no global `StagePlayback` clock.

Build a reverse dependency index from declarations to targets, skeletons, sources
and relevant composition arcs. Reconcile on contract-version, clip-roster, policy,
relationship, time-sample, stage-rate, topology, payload and source changes:

- Preserve clocks, acquired state and completion identity for unchanged bindings.
- Apply changed authored policy only to affected heads; re-clamp changed ranges
  and invalidate their samples. Do not reapply unchanged defaults over transport
  overrides or re-run autoplay on unrelated edits.
- Validate a replacement binding before installing it. If an authored edit makes
  an active binding invalid, suspend that binding, remove its transient source
  opinion, release ownership and publish an error. Do not keep sampling a stale
  removed source or stale target ID.
- Remove managed heads whose declarations/discovery entries disappeared, without
  deleting explicitly programmatic heads. Despawn, unload and stage-generation
  changes cancel pending work and clean up runtime opinions.

### B. Native source switching

**Recommended first implementation: actor-local native binding overrides in a
dedicated transient session-composition layer.** This follows the existing
`UsdSkelSkeletonQuery` evaluation path, including joint mapping and skinning
validation, rather than introducing a second pose evaluator.

1. Resolve each declared target to its editable skeleton binding sites and native
   consumers. Validate source type, joint compatibility using native mapping
   rules, rest/bind requirements and blendshape binding compatibility.
2. On channel acquisition, author the selected `skel:animationSource` at those
   actor-local sites in the dedicated layer. Define layer precedence explicitly
   and verify the effective relationship; an overriding opinion must produce a
   binding diagnostic, not a false successful switch.
3. Partition/rebuild affected native query/dependency caches by actor/SkelRoot.
   Invalidate old and new dependency entries on switches and source edits. Shared
   source edits invalidate all dependent actors; switching one actor does not
   reset another's clock or rebuild the whole stage each frame.
4. Perform channel replacement, source selection and binding validation as one
   controller transition. A failed play request leaves the previous valid action
   active. Only the winning binding contributes samples.
5. Remove the controller's opinions on release, removal, preview teardown or
   shutdown, exposing the current underlying authored binding. Exclude this
   runtime layer from save, undo history, collaboration edits and cook input;
   test packaging while a preview override is active.

Use editable actor wrappers with separately addressable referenced rigs. Reject
instance-proxy binding sites with an actionable diagnostic and provide a sample
composition with editable rig occurrences. Never author on a shared prototype.

### C. Channel and transport semantics

Reuse the six existing SDK methods. Add explicit `release()` to return targets to
the containing timeline; it is distinct from stop-and-rewind. Keep existing wire
command numbers/imports compatible and add new imports/values through the ABI's
additive conventions.

| Action/state | Ownership and result |
| --- | --- |
| Discovered/authored inactive clip | Follows composed animation; no independent sample or source override. |
| `play` | Acquire channel, replace its previous action, select the real source, then start/resume. A completed action restarts at its direction-appropriate boundary. |
| `pause` | Hold an active action's current pose/source. Acquiring a following clip must initialize from the containing clock, clamped to range, rather than silently snapping to the stored start. |
| `seek` | Acquire/select the addressed clip and sample the clamped time; seeking alone emits no completion. |
| `stop` | Stop and rewind the active action, retaining its start pose/source. Stopping an inactive/replaced head resets that head without stealing the channel. |
| `set_speed`, `set_looping` | Update configuration without acquiring an inactive clip or replacing another channel member. This corrects the current unconditional acquisition behavior. |
| Non-looping completion | Hold the terminal pose/source, record completion once per run and notify the owner. The behavior can play idle in response. |
| Replacement | Previous head stops contributing and reports interruption, not completion. A later play can resume it; stop then play provides an explicit restart. |
| `release` | Remove this head's ownership/source override and force a fresh containing-clock sample even if that clock is paused. Releasing an inactive head does not release another action. |

Commands are ordered deterministically using stable attachment order and FIFO per
sender; define editor/autoplay insertion order in the same queue. Validate finite
numbers and clip labels at SDK/host boundaries, rejecting values that overflow
the existing `f32` seek wire representation. Reverse restarts, speed-zero and
large loop steps need explicit regression cases.

### D. Sampling ownership

Resolve **write sets**, not just requested root IDs. Include target descendants,
native skeleton/mesh consumers and domain information. Enforce ownership after
native dependency expansion, so a global ancestor sample or shared source cannot
write an independently owned actor through a different root.

- Use transform-only and skeleton-pose fast paths for eligible clips; retain full
  projection where geometry, visibility or CPU fallback deformation requires it.
  A steady-state joint-only clip should not re-extract static mesh data per frame.
- Apply one winning sample per owned domain. Filter expanded global projections
  or split their write sets; preserving unrelated stage animation is mandatory.
- Keep behavior/physics placement on the actor's outer wrapper, outside animated
  targets. Compose actor-local animated transforms with the current placement
  transform rather than resampling placement at the clip's time.
- Define clip ownership of animated properties while active. A behavior write to
  the same animated domain receives a conflict diagnostic until release; writes
  to unrelated domains continue normally. Effective visibility keeps the existing
  visibility-override policy.
- Hiding an actor does not stop its player or behavior. The sample behavior
  explicitly pauses/stops on Catch. No generalized suspend-on-hidden policy is
  introduced by this contract.

### E. Status and events

Expose a session playback snapshot and a capability-gated, allocation-free SDK
query using caller-owned buffers where strings are needed. Include owner, clip,
channel, effective source, range/rate, current time, ownership, playing/paused/
stopped/completed/invalid state, run ID and last command outcome.

Preserve command origin, attachment generation and a sequence ID in the deferred
host envelope. Existing `Result<()>` means **enqueued**, not applied. Report missing
player/clip, stale target, unresolved binding, invalid range and ownership conflicts
through correlated outcomes and runtime/Conductor diagnostics. Stage-scoped NULL
targets must resolve to an actual owner or return a diagnostic; they must not be
mistaken for broadcast or assumed to discover child clips automatically.

Emit completion, interruption and failure through the existing bounded deferred
event dispatcher. Completion carries owner/clip/channel/run identity and is
generated once on boundary crossing, never on seek, pause, stop or rehydration.
Target the owner's attached behavior for completion; report command failures to
the issuing attachment. Prevent delivery to a recycled/detached attachment and
retain queryable status under event backpressure.

Keep the existing frame order: animation samples before behavior hooks; behavior
commands settle at the next animation tick. Queue completion for a subsequent
safe behavior dispatch, allowing idle → reaction → idle without recursive hooks.

## 4. Conductor and packaging

- Extend the inspector registry with a clip-player contribution for owners of the
  versioned property contract. Edit the roster, relationships, range, speed,
  loop, autoplay and channel through the authoring journal. Extend typed property
  authoring/readback for `timecode` and uniform variability. Native binding-site
  `SkelBindingAPI` application uses the existing single-apply path.
- Show catalog entries together with authored clips, resolved targets/source,
  stage-domain timing, ownership, status and validation errors. Reuse skeleton
  inspection and extract reusable transport controls from the global toolbar.
- Actor preview sends commands to the same controller. It has its own clock;
  release/teardown returns the actor to its composed timeline. Editing source or
  undoing/redoing a declaration reconciles the same native caches as runtime.
- Use the existing [`idtx-build` cook](crates/idtx-build/src/cook.rs) and USD
  dependency discovery. A relationship points to a prim, not an asset file;
  animation files enter the closure through their normal reference/payload/value
  clip arcs. Validate every declared action, including inactive reaction sources,
  and verify both packaged USDZ and supported loose output paths.
- Add reusable idle/reaction rig assets plus a rigid-animation example and a
  small attached behavior with `playback`/`events` capabilities. Include a two-copy
  scene with independent speeds and randomized outer placement, without
  experience-specific dependencies.

## 5. Implementation sequence and reviewable deliverables

1. **Property contract and binding model** — `idtx-usd-sys`, `idtx-usd`,
   `idtx-authoring`, `idtx-scene`. Add shared contract constants/validation,
   typed `timecode` access, roster authoring, authored catalog entries and
   incremental reconciliation. Deliver a USD-open test that creates named
   players without a new animation schema plugin or test-side ECS insertion.
2. **Native binding and sampling ownership** — `idtx-usd-sys`, `idtx-usd`,
   `idtx-scene`, `idtx-studio-session`. Implement isolated source overrides,
   actor-scoped caches, channel arbitration, bounded write sets and sample fast
   paths. Deliver two-rig/source-switch tests with unrelated stage motion.
3. **Observable transport** — `idtx-behavior-abi`, `idtx-behavior`,
   `idtx-behavior-sdk`, `idtx-studio-session`. Extend existing transport with
   release, validation, snapshots and deferred outcomes/events. Deliver a real
   attached WASM behavior completing reaction and resuming idle.
4. **Conductor authoring and preview** — `idtx-editor-ui`, `idtx-studio-shell`,
   `idtx-authoring`. Add inspection, undoable declarations and same-runtime actor
   preview, with effective-source and error reporting.
5. **Cook and acceptance fixtures** — `idtx-build`, native USD/session integration
   tests and sample assets. Verify timing conversion, offline dependency closure,
   removal/lifecycle handling and save/reopen/cook equivalence.

Each step extends the production path and includes its relevant verification;
the end-to-end behavior/runtime case is the completion criterion for F4.

## 6. Acceptance matrix

| Scenario | Required evidence |
| --- | --- |
| Authored start | Open USD with a named declaration and attached behavior in standalone runtime; behavior starts it with no manual player insertion and no required global clock. |
| Discovery compatibility | Existing anonymous animations in the stage and loaded payloads still follow global Play. Catalog refresh alone does not acquire/freeze them. |
| Schema-free composition | Generic OpenUSD opens/retains declarations without an animation plugin. Referenced relationship targets and timecode endpoints remap. Overriding the roster removes inherited clips; malformed contract versions/types are diagnosed. |
| Independent copies | Two referenced rigs use different ranges, speeds and actual native sources. Verify sampled joint matrices/source paths, not only playhead labels. Unrelated animation continues. |
| Shared dependencies | Include two actors using one source and a global animated ancestor. Neither dependency expansion nor global/full projection overwrites independent poses. |
| Channel replacement | Idle → reaction → idle contributes one source per target. Conflicting autoplay/overlapping channels are diagnosed; placement stays unchanged through source switches. |
| Transport | Forward/reverse, speed zero, large loop steps, pause/resume, seek, stop, explicit restart and release work at boundaries. Non-looping completion occurs once per run; other operations do not fabricate it. |
| Reconciliation | Unrelated edits preserve state. Declaration/source/target removal, source replacement, payload unload/reload and actor despawn clear stale heads/caches/opinions and cancel stale deliveries. |
| Timing | Mixed source rates and nonidentity reference offsets produce reported, verified stage-domain ranges before/after cook. Test timecode endpoints authored with the source and in a different override layer, including the `0..48` → `100..196` case. No double application of time offsets. |
| Native compatibility | Native joint mapping, supported skinning methods and CPU fallbacks survive switching. Unsupported blendshape deformation remains a visible diagnostic with bindings preserved. |
| Errors | Wrong source type, missing clip/target, invalid/non-finite range/speed, incompatible joints and instance-proxy targets appear in status, runtime diagnostics and Conductor. |
| Authoring parity | Undo/redo and source edits do not leave stale caches. Save/reopen and cook while previewing retain declarations and exclude transient source selections. |
| Runtime lifecycle | Spawned actors autoplay once when active; hidden actors continue unless explicitly stopped/paused; stage unload/preview teardown releases transient ownership. |
| Performance | Unrelated binding edits do not reset/rebuild other players; steady joint-only playback retains static mesh data and avoids whole-stage cache rebuilds. |

Run targeted scene/SDK/controller unit tests, USD-linked integration tests and
standalone behavior/cook tests for these scenarios using the repository's OpenUSD
test environment. Native SDK stubs returning `Ok(())` are not evidence of host or
source-switch correctness.

## Standards references

- [UsdSkelAnimation](https://openusd.org/release/api/class_usd_skel_animation.html)
  and [native binding schema](https://github.com/PixarAnimationStudios/OpenUSD/blob/v26.08/pxr/usd/usdSkel/schema.usda).
- [Value clips: sequencing, looping, ignored relationships and flattening](https://openusd.org/release/api/_usd__page__value_clips.html).
- [Custom attributes and standard value types](https://openusd.org/release/api/_usd__page__datatypes.html)
  and [customData semantics](https://github.com/PixarAnimationStudios/OpenUSD/blob/v26.08/pxr/usd/usd/object.h).
- [Native timecode endpoints and composition-offset behavior](https://github.com/PixarAnimationStudios/OpenUSD/blob/v26.08/pxr/usd/usdMedia/schema.usda).
- [Collections](https://github.com/PixarAnimationStudios/OpenUSD/blob/v26.08/pxr/usd/usd/collectionAPI.h),
  [variant selection](https://github.com/PixarAnimationStudios/OpenUSD/blob/v26.08/pxr/usd/usd/variantSets.h),
  [stage flattening](https://github.com/PixarAnimationStudios/OpenUSD/blob/v26.08/pxr/usd/usd/stage.h),
  [motion blur](https://openusd.org/release/api/class_usd_geom_motion_a_p_i.html)
  and [spline looping](https://github.com/PixarAnimationStudios/OpenUSD/blob/v26.08/pxr/base/ts/spline.h).

## Approved scope

The approved implementation includes:

1. Extending the existing catalog/hydration/SDK with the schema-free, versioned
   property contract, native `timecode` endpoints and defaults above.
2. Actor-local transient native binding overrides with scoped cache invalidation.
3. Explicit release-to-timeline, non-acquiring configuration commands, exclusive
   replacement channels and deferred, queryable outcomes.
4. Preserving current native skinning capabilities and blendshape diagnostics;
   adding blendshape deformation, blending or crossfades would be separate scope.
