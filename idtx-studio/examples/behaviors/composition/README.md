# Authored behavior composition

Open `two_copies.usda` in Conductor. Each experience references the same task
panel and coordinator assets. Its `coordinator` relationship resolves to its own
coordinator, including after renaming/reparenting `Hunt`.

In the **Behavior** inspector, use **Authored context and targets** to edit JSON
and select composed targets. Apply creates a reference-local override, with
undo/redo and normal stage save/reopen. Edit Code, Cargo Check and Verify
capabilities use the existing source workflow. Compile/cook before runtime use.

The panel posts the authored `hunt.start` event on its Start widget callback.
The coordinator makes one idle-to-running transition. `pointer.rs` and `menu.rs`
are additional templates: attach them using Create/Attach Behavior, author a
`coordinator` relationship and context (`{"event":"hunt.targeted"}` for the
pointer; `{"reset":"hunt.reset","help":"hunt.help"}` for the menu). The menu
expects Reset/Help children. The pointer declares `widgets.click` and `select`
input actions. It handles widgets **before** world selection and returns when
consumed, so a shared physical button cannot also select the world.

## Runtime contract

- `events::post(entity, name, bytes)` requires `events`, rejects NULL and takes
  opaque bytes (name ≤256 UTF-8 bytes, payload ≤64 KiB). Name + payload retained
  capacity must also fit the host's configurable argument/queue budget (64 KiB
  by default). Oversize/queue-full/bad arguments return `InvalidArgument`;
  stale handles return `StaleHandle`; unavailable targets return `NotFound`.
  `events::broadcast` is the deliberate broadcast entry point.
- Normal dispatch runs input/update/late-update, collects events FIFO per sender
  in attachment order, then widget callbacks. Posts made by delivered callbacks
  wait for the next collection sweep. Previously queued work can be delivered
  earlier in a subsequent frame. Deferred USD/session edits apply **after**
  dispatch, so a callback sees one coherent snapshot. Under overload the host
  rotates phases for fairness, retains ordered continuations, and shares its
  hook/fuel/time/recipient budget across all work. Cascades never recurse; their
  pending items/bytes and per-frame execution are bounded by existing host limits.
  Queued success is not an acknowledgement; unload/removal cancels delivery.
- `scene.relationship_targets(entity, name, output)` requires `scene_query`.
  It reports revision, total count, written count, truncation and native forwarding
  resolution status. Ordered entries distinguish live, invalid and unloaded
  targets. A missing relationship is `NotFound`; an empty one succeeds. Only
  live entries provide handles. Re-query to observe edits; reads share a cache
  updated outside callbacks on composition/relationship changes, never live USD
  calls per behavior per frame. Native `GetForwardedTargets` defines forwarding,
  order and composition, including its incomplete-chain reporting.
- `Ctx::context` remains immutable attachment parameters (≤8192 UTF-8 bytes).
  Authored changes detach/recreate the instance at the next reconciliation
  boundary, reset mutable behavior fields, and re-run attachment/start hooks.
  Malformed, wrong-typed or oversized context blocks attachment with a diagnostic;
  it is never truncated or replaced by `null`. Runtime fields belong in the
  behavior struct. Existing custom string context opinions remain valid after
  schema registration. Arbitrary customData keys are not automatically hydrated;
  only the existing `idtx` snapshot is available through `Scene::custom_data`.

Use native USD relationships for objects and `UsdCollectionAPI` when authored
sets need collection membership semantics. The SDK relationship query returns
targets, not collection membership expansion. JSON strings are never expanded
into paths. Event names above are project data, not engine vocabulary.
