# Handbook validation record

The SDK/API edition was refreshed on **23 November 2026** against implementation revision `08c7fb0`, for a developer with packaged Conductor, a matching SDK/Runtime and this book. Project schema: **1**; runtime manifest schema: **2**; guest ABI: **3**. That refresh covers relationships, curves, context reattachment, modal/source-specific input, widgets and named clip playback/status/events. No engine source checkout or engine test helper is a reader prerequisite.

## Behavior authoring guidance update — 26 November 2026

The authoring workflow was reviewed against implementation revision `546ec2c`. The update documents starting every edit with the Inspector’s **Create Behavior…** or **Edit Code**, managed Cargo/VS Code workspaces, the selected VS Code profile, rust-analyzer setup, `rust-src`/`rustfmt` prerequisites, workspace regeneration, helper modules, source sharing and local copies. It also corrects the source-wrapper description: authored entries are normal Rust modules. The downloadable Signal Room instructions and their complete listings carry the same workflow.

Both handbook checks passed for this update: `python tools/check_book.py` checked **35 Markdown files**, **265 local links**, **17 matching project listings**, balanced fences and complete navigation; `python tools/create_example.py --check` confirmed all **13 Signal Room files** match their complete listings.

This is a documentation and implementation review. It does not add a live Conductor/VS Code acceptance run or rerun the SDK compilation suites. The SDK and runtime evidence below remains attributed to its original review. Editor acceptance still requires opening a behavior from the Inspector, confirming SDK completion and diagnostics, saving a change and running Inspector checks with the installed distribution.

## Shared module example — 26 November 2026

The [shared Rust module exercise](../tutorial/07-shared-modules.md) adds two behavior entries, one shared helper and a USD extension layer, with matching companion files and complete listings. Validation used an isolated copy of the source collection and a freshly staged current SDK:

- Both entries passed the managed Cargo development-context check for `wasm32-unknown-unknown` and compiled to WASM through Conductor's behavior build library.
- Changing only the copied helper's `SPEED_SCALE` from 1.0 to 2.0 changed the compiled WASM bytes for **both** consumers. The delivered helper remains at 1.0.
- Native USD flattening of Hub and a composed level wrapper retained both new bindings, resolved their source paths and preserved the base Signal Room content.
- The book checker passed **37 Markdown files**, **274 local links** and **21 matching project listings**. The Signal Room extractor check still passed all **13 base-project files**.

These checks establish module resolution, shared-source rebuilds, guest compilation, composition and documentation consistency. VS Code completion/navigation and visible rotation/speed changes still require the interactive checkpoints in the exercise; they were not exercised by this validation.

## SDK/API edition checks — 23 November 2026

| Check | Evidence and scope |
|---|---|
| Portable project listings | All **13 files** match the companion Signal Room project. The optional extractor reconstructs them in a separate directory and refuses to overwrite authored files. |
| Current SDK unit suite | **35 passed**, no failures. The 8 SDK documentation examples remain ignored; their configuration does not execute them. |
| Updated SDK reference | All **21 Rust blocks** passed native checking and WASM compilation against a freshly staged current SDK. This verifies signatures/guest compilation, not live host effects. |
| Named clip extension | All **4 authored listings** match their companion files. Both new behaviors passed native checking and WASM compilation against the current staged SDK. |
| Widget extension | The complete radial-menu behavior passed native checking and WASM compilation against the current staged SDK; its input TOML parsed. |
| Native USD composition | Fresh Hub/preview and Lab/cooked-shape assemblies remap the Console beacon relationship. The Hub extension also remaps actor and idle/reaction clip-target relationships to the referenced occurrence. |
| Markdown portability | The book checker validates local links, heading fragments, fences, complete navigation and confinement of links to this book directory. The same check is run on a copy outside the Studio directory. |
| Isolated example compilation | The updated Console, Pointer and SpawnDemo passed native checking and WASM compilation outside the repository, using the reconstructed project and a self-contained current SDK copy. Cached Cargo dependencies were used. |
| Documented native harness | The exact Cargo harness listed in the testing chapter passed all **3 Console logic tests** against that isolated SDK. |
| Coding-agent example | The guide’s complete ExhibitToggle behavior passed native checking and WASM compilation against the self-contained SDK. Its TOML and attachment consistency were also checked. |
| Bundled SDK packaging | The Conductor packaging code stages the existing self-contained SDK workspace in Windows and macOS resource layouts. Its focused staging test passes for both layouts. This does not imply every previously distributed app has been rebuilt with that fix. |
| Behavior API review | Public signatures, macro hooks, capability groups, native stubs and deferred operations were checked. Empty playback labels are documented as rejected by the current host; completed/interrupted/failed are the implemented animation notices. Catalog enumeration and marker subscriptions remain absent. |
| Workflow review | Project, Behavior inspector, Preview and Build labels and target tool/runtime requirements were checked against the implementation. |

The book’s Python checks can be run from the **handbook directory** without an engine checkout:

```sh
python tools/check_book.py
python tools/create_example.py --check
```

The [testing chapter](../testing.md) supplies a complete optional Cargo harness for the Console’s pure tests, using the installed SDK. Conductor’s checks and runtime acceptance remain the normal reproduction route.

## Earlier editorial evidence and included images

During preparation of the original example, the SDK unit suite passed 34 checks, 15 complete SDK guide examples compiled natively and as WASM guests, and the Console’s three logic tests passed. Console, Pointer and the optional SpawnDemo compiled through the behavior compiler. Live USD checks opened Hub/Lab and checked typed context overrides; an optional spawn layer composed successfully.

The [Hub](../images/signal-room-hub.png) and [Lab](../images/signal-room-lab.png) images are actual desktop Runtime renders captured during that editorial work. Those capture wrappers provided a stationary camera and no complete player/transition registry. They show initialized readouts and room overrides; they do not prove interactive travel or device delivery. The [widget illustration](../images/widget-readability.png) is a renderer regression image and is captioned as such.

These earlier checks used editorial development tools. They support the examples’ provenance and do not require the reader to obtain those tools. The portable revision is validated separately against its included files and packaged SDK layout.

## Acceptance still requiring interaction or hardware

Follow [testing](../testing.md) and the [interaction walkthrough](../tutorial/04-interactions.md) with your installed distribution. Record:

- Conductor UI checks and runtime pointer/widget input with the intended player camera.
- The cooked Hub → Lab → Hub round trip and carried state.
- Runtime specimen spawning after enabling its layer and registry entry.
- Named clip application/status, completion recovery, pause/release and retargeting during a real running session.
- Radial/polygon hit clipping, background clicks, independent pointer sources and modal ownership.
- Relocation and launch of the complete finished package without authoring paths.
- Real PCVR, Quest and Vision Pro installation, input, lifecycle and sustained performance.

A source compile, SDK staging test, USD composition or selected render does not establish these outcomes. This revision has not performed a complete packaged-app UI tour or physical-headset acceptance. The [capture guide](screenshots.md) identifies the useful remaining UI/device pictures.

The delivered artifact is the complete Markdown book directory, including its example and images. See the [README](../../README.md) for optional HTML export and Mermaid integration.

An optional mdBook HTML build was not run because mdBook is unavailable in the validation environment. The earlier SDK-packaging staging test passed; the API refresh changes documentation, examples and book-validation tooling.
