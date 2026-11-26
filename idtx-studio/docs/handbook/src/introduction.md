# Start here

An IDTX experience starts with USD assets and level stages. Conductor adds a project manifest, behavior attachments, input bindings and player composition. Cooking converts those saved sources into a runtime payload. Packaging combines the payload with a supplied Runtime for a desktop or headset. This book guides you through those steps using the installed Conductor app.

You should be comfortable editing files and reading basic Rust. You can begin without USD expertise: the world-authoring chapters introduce the conventions used by the example. Commands state their working directory and use installed executables or files supplied with this book. The [setup chapter](setup.md) explains the few external tools experience authors need.

## The experience you will build

**Signal Room** has two rooms, Hub and Lab. Both reference a reusable USD station with a beacon and interactive console. Behaviors rotate the beacon, route pointer clicks, highlight the object under the pointer and carry the run/pause choice when the visitor changes rooms. An optional exercise spawns a registered specimen and handles its readiness callback.

| Milestone | What you learn | Checkpoint |
|---|---|---|
| Open Hub | Project roots, USD metadata and reusable references | Correct hierarchy and metre-scale geometry |
| Attach behaviors | Lifecycle, typed context, capabilities and compilation | Behavior checks pass; beacon rotates |
| Add interaction | Logical input, camera rays, widgets and transient feedback | One toggle per click; green beacon outline |
| Compose the player | Source, preview and cooked namespaces | Camera and desktop/XR controls work |
| Change levels | Cooked registry, teardown and carried state | Hub → Lab → Hub retains run/pause choice |
| Spawn a specimen | Prototype registration and deferred readiness | Ready callback; spawned descendants resolve |
| Validate | Compile, runtime, relocation and device checks | Recorded results for each boundary |
| Deliver | Target Runtime and packaging tools | Finished package launches on its target |

The [complete project](example/index.md) contains every required authored file as both companion files and full listings. There is no project-specific engine helper. Follow [the walkthrough](tutorial/01-example-tour.md) to understand and customize it, then keep the [SDK reference](reference/sdk-overview.md) open while authoring more behaviors.

## Choose a reading route

- **First experience:** follow the numbered chapters in order, using Signal Room at each checkpoint.
- **Behavior author:** always start editing in Conductor’s **Inspector > Behavior** with **Create Behavior…** or **Edit Code**. Follow the [workspace walkthrough](tutorial/03-behaviors.md#start-every-edit-from-the-inspector) to get rust-analyzer completion and diagnostics, then read the lifecycle, host contracts and SDK reference.
- **Experience QA or release engineer:** read testing, cooking, target packaging and operations.
- **Coding agent:** read the [agent contract](reference/agent-guide.md), complete project listings and project AGENTS.md before proposing edits. Those documents establish APIs and project ownership without an engine checkout.

## Match the book to your distribution

The SDK/API edition was checked on **23 November 2026**; the Inspector and behavior workspace guidance was updated on **26 November 2026**. It describes project schema **1**, runtime manifest schema **2** and behavior ABI **3**. Use the SDK shipped with your Conductor distribution and compatible supplied target runtimes. If a documented control or API is missing, check the release with your distributor rather than adding an engine build dependency to the experience.

Native SDK wrappers are inert stubs. A native test can prove a calculation or JSON decoder works; runtime acceptance must prove that a host operation affects the scene. Editor behavior playback and animation transport are separate. Physics runs in Runtime preview and delivered players. Active-level preview does not establish multi-level travel; the cooked package supplies the complete registry.

The current SDK provides composed relationship queries, BasisCurves sampling by metre distance, source-aware input and modal capture, panel-background widget callbacks, named events, and animation transport with release and settled status. Authored named clips emit lifecycle events. Clip catalog enumeration, marker subscriptions and arbitrary attribute watches remain unavailable. Target functionality depends on the Runtime supplied in your distribution. A project setting cannot add compiled support to that Runtime.

After the base walkthrough, follow the [animation and event-routing exercise](tutorial/06-animation-and-routing.md), then use the [widget reference](reference/widgets.md) for radial/polygon panels and source-aware interaction. The [SDK reference](reference/behavior-sdk.md) covers the exact relationship, curve, capture and playback contracts.

The [compatibility map](appendices/implementation-map.md) describes those dependencies, and the [validation record](appendices/validation.md) distinguishes editorial checks from UI and device acceptance.

**Next:** [Understand the system](system.md).
