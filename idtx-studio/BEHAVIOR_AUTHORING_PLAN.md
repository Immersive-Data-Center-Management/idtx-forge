# Behavior authoring: VS Code, Rust analysis, and an embedded editor

**Status:** proposed implementation plan, based on the current code and a local feasibility experiment.

**Date:** 2026-11-25. Paths below are relative to `idtx-studio/`.

## 1. Recommendation

**Start with a generated Cargo development context and the existing VS Code editor.**
Conductor should open a named `.code-workspace` containing the author's real source
files, and configure rust-analyzer to use Conductor-managed Cargo manifests. The
author gets normal Rust completion, navigation, refactoring, formatting, and
compiler diagnostics while Conductor owns the compilation recipe.

The recommended order is:

1. Generate a persistent editor workspace; provide the correct SDK, target,
   procedural macros, dependencies, and compilation settings.
2. Make the generated Rust wrapper suitable for completion, using the same module
   structure in the editor and production compiler. The experiment below found
   this requires attention beyond simply exposing `Cargo.toml`.
3. Support ordinary Rust source folders and modules, including folder-aware
   create/import/duplicate operations.
4. Add a small VS Code companion extension when integrated Check / Verify /
   Preview commands and exact window routing justify it.
5. Consider an embedded editor after measuring the remaining workflow friction.

The first useful VS Code release is approximately **1–2 engineer-weeks**; a rounded
multi-file workflow is approximately **2–4 engineer-weeks total**. A production
embedded editor is more plausibly **8–16 engineer-weeks**, with substantially more
ongoing maintenance. These are planning ranges for an engineer familiar with the
codebase, not measured implementation costs; compatibility requirements can move
them upward.

## 2. What already exists

| Area | Current implementation | Consequence for the plan |
|---|---|---|
| Create / Edit | `crates/idtx-studio-shell/src/behavior_tools.rs`: `create_behavior_source`, `edit_behavior_source`, `launch_vscode` | Creates a stage-relative `behaviors/<stem>.rs` and launches `code --new-window --profile "IDTX Behavior" <file>`. There is no editor workspace. |
| Attach / import | `behavior_tools.rs`: `attach_behavior_source`; `crates/idtx-studio-shell/src/render.rs`: `perform_attach_behavior` | The picker selects one `.rs`; import copies only that file. Companion files are lost on import. |
| Generated crate | `crates/idtx-behavior-build/src/skeleton.rs` and `skeleton/` | Already generates an isolated Cargo package, a `#![no_std]` wrapper, and a copied `src/behavior.rs`. The wrapper uses `include!("behavior.rs")`. |
| Actual build settings | `skeleton/Cargo.toml.tmpl`; `src/compile.rs` | Guest edition **2021**, `cdylib`, `wasm32-unknown-unknown`, release profile, `panic = "abort"`, direct SDK and `arrayvec` dependencies. The surrounding Studio workspace uses edition 2024; opening that workspace is not the correct guest context. |
| Partial multi-file support | `skeleton.rs`: `stage_companions` | Copies direct sibling `.rs` files, excluding the entry and reserved `lib.rs` / `behavior.rs` names. It does not recursively stage source folders or non-Rust include data. This is a useful starting point, not a complete multi-file contract. |
| Check / Verify | `behavior_tools.rs`: `start_check`, `start_verify` | Background Cargo Check and build-plus-capability-link verification already exist. Rust analysis should complement these operations. |
| Preview / packaging | `crates/idtx-studio-shell/src/behavior_prepare.rs` | Resolves the source through USD, compiles it, and authors a compiled-module overlay. Source resolution must continue to respect the layer that authored the reference. |
| SDK distribution | `crates/idtx-behavior-build/src/sdk_sysroot.rs` | Packaged Conductor already ships the SDK, ABI, macros, inheritance workspace, and lock seed. Editor integration can reuse this without a Studio checkout. |
| Cache correctness | `crates/idtx-behavior-build/src/lib.rs`, `fingerprint.rs`, `tests/cache_contract.rs` | The cache accounts for resolved dependencies, configuration, tools, staged content, and artifact receipts; Cargo also validates dynamic inputs. Preserve this contract when extending source discovery. |
| UI / command surface | `crates/idtx-studio-shell/Cargo.toml`; `apps/idtx-conductor/src/main.rs` | Native egui/winit/wgpu application. There is no existing embedded web editor or behavior-editor LSP client. The Conductor CLI currently opens projects. `cargo xtask build-behavior` exists for repository development. |

**Two immediate integration gaps:**

- The original file opened in VS Code and the copied file compiled by Cargo have
  different paths. Exposing the existing scratch crate alone would analyze and
  navigate to the copy. Editing that copy would also be overwritten on preparation.
- VS Code creates an **empty profile** when an unknown `--profile` name is used.
  Installing rust-analyzer in the user's usual profile does not establish that it
  is available in `IDTX Behavior`. Profile setup is part of this feature.[R1][R2]

## 3. Feasibility experiment and its design impact

An isolated temporary project was exercised with the real SDK from this checkout,
Rust **1.96.1**, and the installed VS Code extension's rust-analyzer
**0.3.3121**, on macOS ARM64. A small LSP client opened the original source outside
the generated package using `linkedProjects` and `vfs.extraIncludes`.

The fixture used `#[behavior]`, `Ctx`, a sibling `helpers.rs`, and a nested module.

| Probe | Literal `include!("/absolute/original.rs")` wrapper | `#[path = "/absolute/original.rs"] mod behavior;` wrapper |
|---|---|---|
| SDK hover and go-to-definition | Worked | Worked |
| Helper navigation and cross-file rename | Worked | Worked, targeting the original author files |
| `#[behavior]` expansion | Worked | Worked, including generated ABI exports |
| SDK method completion | Returned no items in the tested requests | Returned SDK methods such as `scene()`, `physics()`, and `camera()` |
| Completion in an unsaved `ctx.` / `ctx.sc` expression | Returned no items | Worked, including inside the annotated impl |
| Recursive source layout | Cargo Check succeeded | Cargo Check and a full WASM build succeeded |

With the module wrapper, an intentional type error in the original helper produced
`E0308` with the original file and range, both in Cargo JSON and in rust-analyzer's
LSP diagnostics. The built WASM contained `_idtx_abi_version`, `_idtx_on_update`,
and the other expected macro-generated exports.

**Decision:** use the normal-module wrapper as the preferred implementation path.
Do not accept successful hover alone as proof that completion works. The observed
`include!` limitation is specific to this tested setup/version; reproduce it in the
supported VS Code versions before deciding whether a compatibility workaround is
needed. Remaining validation includes the actual VS Code launch/profile flow,
Windows, and the full behavior corpus.

## 4. The authoring contract

### Author-owned and Conductor-owned inputs

| Author-owned | Conductor-owned |
|---|---|
| Behavior entry source; helper modules; shared source libraries; declared compile-time data files | Cargo manifests; wrapper crate; SDK selection; dependency/version policy; lock resolution; WASM target; compilation flags; generated editor configuration |
| Behavior configuration through the existing inspector | Translation of that configuration into checks, build inputs, and runtime metadata |

The runtime manifest remains on `IdtxBehaviorAPI` attributes. A source-folder
feature does not require reintroducing a capabilities/inputs sidecar.

### Keeping the outer project managed

Use a combination of:

1. A source-focused Explorer: only authoring folders are workspace roots.
2. `files.readonlyInclude` for generated files and the selected SDK source tree;
   keep SDK navigation available for learning and documentation.[R4]
3. Generated-file headers and a **Regenerate Editor Workspace** action.
4. A compiler that materializes its canonical recipe from Conductor inputs, rather
   than treating an edited IDE manifest as an authoritative build configuration.

This meets the normal-workflow requirement: developers edit source and cannot
accidentally customize the build through the presented editor. Read-only settings
and hidden files are UX controls; the same local OS user can override them. Strict
filesystem enforcement would require a different execution/permission boundary
and is unnecessary for the stated developer workflow.

If additional dependencies become a requirement, expose a small product-level
dependency configuration and generate Cargo from it. Keep SDK/ABI pins under
Conductor's control. Ordinary helper modules need no new dependency mechanism.

## 5. Recommended managed-workspace design

### 5.1 Persistent Cargo context, real author files

An illustrative layout supporting both existing sources and later source folders:

```text
project/
  project.toml
  levels/main/
    scene.usda
    behaviors/
      existing_spin.rs             # existing source can retain its location
      spin/
        behavior.rs                # proposed new-behavior layout
        motion.rs
        motion/easing.rs
      shared/
        math.rs
  .idtx/behavior-ide/
    behaviors.code-workspace       # generated, machine-local
    target-ra/                     # stable analysis-only output
    crates/<source-key>/
      Cargo.toml
      Cargo.lock
      src/lib.rs                   # generated module wrapper
```

The editor wrapper would contain:

```rust
#![no_std]

#[path = "/absolute/project/levels/main/behaviors/spin/behavior.rs"]
mod behavior;
```

Generate the path as an escaped Rust string. Use absolute, normalized paths in the
machine-local configuration where they avoid multi-root ambiguity; regenerate
after a project or SDK move. Keep portable source references in USD layer-relative.

Advantages:

- Completion and diagnostics operate on the file actually being edited, including
  its unsaved buffer. No source synchronization or diagnostic URI proxy is needed
  for editor checks.
- Normal `mod motion;`, nested modules, and relative include paths resolve from
  the authoring tree.[R8]
- Creating or editing a helper does not require regenerating Cargo files.
- A saved editor workspace continues to work with Conductor closed.

Use stable identities derived from the project/source location and recipe identity,
not the source's current contents or the selected prim. Multiple attachments of one
source should share one development context. Keep analysis directories stable
across keystrokes and ordinary saves.

### 5.2 Share the recipe with the production compiler

Refactor preparation in `idtx-behavior-build` into a reusable description of:

- entry path and supported source roots;
- source format / wrapper mode;
- selected SDK and resolved lock/dependencies;
- guest edition, target, features, profile, toolchain, and relevant environment;
- stable source identity and generated-file locations.

Expose an editor-preparation API that can run independently of producing WASM.
The shell should call it off the UI thread before launching VS Code. Reuse existing
SDK discovery, lock seeding, and write-if-changed behavior.

**Recommended binding strategy:** the editor wrapper points to live author files;
production preparation stages a source snapshot and generates the equivalent
module wrapper pointing into that snapshot. Preserve filenames and relative
directory structure. Both paths must have the same Rust module semantics and
dependency/target settings.

Document compile-time path semantics: `CARGO_MANIFEST_DIR` identifies the managed
crate, not the author's folder. Prefer source-relative includes, and cover
path-sensitive `env!` / `file!` usage in parity checks between live and staged inputs.

Production snapshotting preserves the existing content-fingerprint approach. Simply
changing the production shim to reference arbitrary files outside `scratch/src`
would require extending the hash: `fingerprint.rs` currently hashes the root
package's staged `src` tree. Cargo freshness by itself is not a replacement for that
content identity. Keep the existing dynamic-input validation and changed-input
detection as well.

For compiler errors from snapshots, retain an original-to-staged path map or use
appropriate path remapping. Preserve line/column positions by copying source
verbatim. A helper CLI exposing check diagnostics later should retain structured
Cargo JSON, rather than only the rendered strings currently returned by the build
library.

### 5.3 Wrapper compatibility is a release checkpoint

Changing `include!` at crate root into `mod behavior` changes the Rust namespace.
For example, an author's `crate::MyType` becomes `crate::behavior::MyType`;
`module_path!()`, visibility assumptions, and some macro paths can also differ.
The sample/built-in behavior scan found no `crate::` references, but that is not
proof about existing customer sources.

Plan the migration explicitly:

1. Test all shipped behaviors and representative user behaviors with the new
   wrapper, including actual ABI/link verification.
2. Define the source contract as a module body. Recommend `self::` / `super::`
   relative paths inside reusable author code.
3. Use a versioned source format when retaining legacy compatibility. A single
   project-level authoring-format version is the lowest-overhead starting point;
   absence means legacy. Keep this version in persisted project authoring metadata,
   not only in disposable generated files.
4. New projects use the module format. Provide a project-upgrade operation that
   checks sources and reports required changes before switching existing projects.
   Individual entry files need not move just to adopt the module wrapper.
5. Pass the chosen format consistently to Edit, Check, Verify, Preview, cook, and
   command-line builds. Legacy sources can continue to build with the old recipe
   while their upgrade is pending.

A blanket `pub use behavior::*` is not a complete compatibility solution for private
items. An editor-only namespace change would also produce misleading diagnostics;
use the same selected format for analysis and compilation.

### 5.4 rust-analyzer configuration

Example generated workspace for one behavior; `/project` and `/sdk` are illustrative
absolute paths. For multiple behaviors, add the corresponding manifests and source
folders. Generate SDK-specific paths from the same discovery result as the compiler.

```json
{
  "folders": [
    { "name": "Behavior Sources", "path": "/project/levels/main/behaviors" }
  ],
  "settings": {
    "rust-analyzer.linkedProjects": [
      "/project/.idtx/behavior-ide/crates/spin/Cargo.toml"
    ],
    "rust-analyzer.vfs.extraIncludes": [
      "/project/levels/main/behaviors"
    ],
    "rust-analyzer.cargo.target": "wasm32-unknown-unknown",
    "rust-analyzer.cargo.allTargets": false,
    "rust-analyzer.check.allTargets": false,
    "rust-analyzer.cfg.setTest": false,
    "rust-analyzer.cargo.cfgs": ["!debug_assertions", "!miri"],
    "rust-analyzer.procMacro.enable": true,
    "rust-analyzer.cargo.buildScripts.enable": true,
    "rust-analyzer.cargo.extraArgs": ["--locked"],
    "rust-analyzer.checkOnSave": true,
    "rust-analyzer.check.extraArgs": ["--release", "--lib"],
    "rust-analyzer.cargo.targetDir": "/project/.idtx/behavior-ide/target-ra",
    "rust-analyzer.cargo.buildScripts.overrideCommand": [
      "cargo", "check", "--locked", "--release", "--lib", "--keep-going",
      "--target", "wasm32-unknown-unknown",
      "--target-dir", "/project/.idtx/behavior-ide/target-ra",
      "--message-format=json"
    ],
    "rust-analyzer.imports.preferNoStd": true,
    "files.readonlyInclude": {
      "/project/.idtx/behavior-ide/**": true,
      "/sdk/**": true
    },
    "files.exclude": { "**/.idtx": true, "**/.behavior-cache": true },
    "search.exclude": { "**/.idtx": true, "**/.behavior-cache": true },
    "[rust]": { "editor.defaultFormatter": "rust-lang.rust-analyzer" }
  },
  "extensions": { "recommendations": ["rust-lang.rust-analyzer"] }
}
```

Important implementation details:[R5][R7]

- Materialize and resolve the lockfile before enabling `--locked`; an SDK workspace
  lock seed may need adjustment for the generated package.
- Configure the **guest** target even on desktop hosts. Several SDK/macro APIs are
  `cfg(target_arch = "wasm32")`; host analysis can silently omit relevant code.
- `#[behavior]` is a real procedural macro. Keep macro expansion and host-side
  macro compilation enabled; suppressing the macro would lose generated contracts.
- Release checks and initial macro/build-script compilation should use compatible
  settings. `--release` belongs in check/build-script commands, not indiscriminately
  in `cargo.extraArgs`, which also affects `cargo metadata`.
- Match rust-analyzer's extra cfgs to the recipe. The example removes its default
  `debug_assertions` / `miri` additions for this release-oriented context. Generate
  actual cfg/features from the supported configuration rather than promising full
  compiler equivalence from this illustrative file alone.
- Use the resolved Cargo/toolchain and environment. GUI launches and an already
  running VS Code process can have different environments; a new window or profile
  does not guarantee inheritance from Conductor. Prefer explicit configuration of
  the relevant tool paths/environment over depending on launch-time `PATH` changes.
- Keep analysis targets separate from production artifact directories. This avoids
  most build-output contention; Cargo registry/lockfile operations may still share
  resources. Linked packages may serialize on their shared analysis target; split
  these directories only if measurements warrant a per-package invocation strategy.
- Do not put source folders or the generated wrapper in `rust-analyzer.files.exclude`.
  Explorer hiding and language-server source visibility are different concerns.

The Rust prerequisites are the installed toolchain, the WASM target, `rust-src`,
and `rustfmt`. The VS Code rust-analyzer extension supplies its own server binary;
the experiment confirmed that an extension can work even when the `rustup`
`rust-analyzer` component is not installed.

## 6. VS Code integration alternatives, from simpler to more involved

### A. Generated workspace + stock rust-analyzer — recommended first

No IDTX extension is necessary for the basic language features.

**Storage variants:**

- **Co-located generated crate:** put a managed `Cargo.toml` and wrapper in each
  behavior source folder, hide/mark them read-only, and open that folder normally.
  This has the simplest Cargo discovery and is attractive for new folder-based
  sources. Existing flat sources need a layout migration or separate generated
  packages.
- **Hidden managed crates with `linkedProjects`: recommended for the current
  layout.** It preserves existing source locations and presents only source
  folders. The additional configuration is small and the external-source link was
  exercised in the experiment.

**Launch behavior:**[R1][R3]

```text
code --profile "IDTX Behavior" /project/.idtx/behavior-ide/behaviors.code-workspace \
     --goto /project/levels/main/behaviors/spin/behavior.rs:1:1
```

- Use a stable workspace path, and allow VS Code's existing-workspace handling to
  reuse it. Reserve `--new-window` for explicitly requesting a separate window.
- `--reuse-window` and `--add` target the **last active** window, not a guaranteed
  matching Conductor project. Validate repeated-open behavior on supported OSes;
  exact session/window selection is a reason for the later companion extension.
- `--goto` can also open a diagnostic at its file/line/column.
- `--wait` waits for files to close; it is not a save notification or a compile
  trigger. Use saved files/check-on-save for the first workflow.
- Keep platform-specific VS Code executable discovery and add a configurable
  executable. Check launch results and surface actionable setup errors. Pass
  paths as separate arguments, including on Windows and for paths with spaces.
- Extend the editor override contract with `{workspace}` alongside `{file}`;
  use structured arguments or correct quoting instead of the current
  `split_whitespace` approach.
- A `vscode://file/...` URI is useful for a fallback or diagnostic link, but a
  bare file URI does not establish the managed Cargo context or select a profile.

**Profiles and setup:**

- Keep the existing dedicated profile as a supported default, with an option to
  use an existing user profile for familiar keybindings/extensions.
- Check rust-analyzer availability in the selected profile. Provide a setup
  action using the supported CLI, for example:
  `code --profile "IDTX Behavior" --install-extension rust-lang.rust-analyzer`.
- Workspace recommendations are recommendations, not automatic installation.
- A `.code-profile` can seed a curated experience. `--user-data-dir` plus an
  extension directory provides stronger instance isolation but adds separate
  settings/extension management; use it only if profile-level configuration proves
  insufficient. Neither is a filesystem restriction.
- Show preparation/indexing/setup status and the resolved SDK/toolchain. Account
  for VS Code Restricted Mode when diagnosing unavailable Cargo/macro execution;
  it is a setup state, not a replacement for managed-file controls.

### B. Project-wide multi-root workspace and generated tasks — small next step

Use one VS Code workspace per Conductor project, with named source roots for the
relevant levels and shared libraries. Start by linking selected/registered behavior
entry points and grow that list as needed. Helper `.rs` files are modules, not
independent behavior crates.

Initially a list of standalone managed Cargo manifests minimizes compiler changes.
If repeated metadata/check work becomes expensive, use one generated Cargo
workspace with one `cdylib` member per behavior and shared dependencies. That
requires adjusting the current standalone `[workspace]` template and placing
profiles/lockfile at the workspace root. Do not put all `#[behavior]` impls in one
crate: their exported ABI names would collide.

Generate ordinary `process` tasks for Check and, optionally, Clippy. Use Cargo's
human-readable output with the rust-analyzer extension's `$rustc` problem matcher,
or feed JSON through a dedicated integration. Rust-analyzer already consumes Cargo
JSON for check-on-save. Avoid two automatic check pipelines doing the same work.

A raw `cargo build` task on an IDE crate produces a development artifact; official
publication, capability Verify, and Preview should use the Conductor build path.
`cargo xtask build-behavior` can help repository developers, but packaged users
should not need the Studio repository or xtask.

### C. Small IDTX companion extension — best next investment in convenience

Keep native VS Code text editors and the official rust-analyzer extension. Add:

- a behavior tree grouped by source, level, and attachments;
- **Create Module**, **Duplicate Behavior**, **Check**, **Verify**, **Preview**,
  and **Regenerate Workspace** commands;
- SDK/toolchain/build status and links to SDK documentation/examples;
- source-location links from Conductor, plus a `registerUriHandler` deep link;
- `saveAll` before a requested preview and clear build results in VS Code;
- optional context-sensitive snippets and later scene/input-name suggestions.

Use the standard Tree View, Task Provider, command, and document APIs.[R9][R10]
Declare the rust-analyzer extension dependency if its presence is required.

A first extension can invoke a small shipped headless helper around
`idtx-behavior-build` for preparation/check/build, with structured output. Such a
helper is **new work**; the current Conductor CLI does not implement these commands.
A separate console helper is useful on Windows, where the shipped Conductor is a
GUI-subsystem executable. Add local IPC to the running Conductor only for actions
that need its session, such as preview or selecting an attachment. Identify the
project/session explicitly so multiple Conductor instances route correctly.

This offers much of an integrated editor experience while reusing VS Code's text
editing, Git integration, search, accessibility, undo, and extension ecosystem.

### D. Generate `rust-project.json` — useful fallback, more work than Cargo here

rust-analyzer accepts an explicit crate graph without Cargo.[R6] This can describe
the original entry as a crate root and, with the supported schema, apply
`crate_attrs: ["no_std"]`. It is an interesting fallback if preserving legacy
crate-root semantics while avoiding `include!` completion problems is essential.

Conductor would need to supply the resolved transitive dependency graph, editions,
target cfgs, features, sysroot sources, environment/build-script outputs, and paths
to **host-built** procedural-macro libraries. The graph should be derived from the
existing build resolution, not hand-maintained SDK stubs.

Flycheck/compiler diagnostics are disabled by default for JSON projects. Supply
`check.overrideCommand` or a supported flycheck runnable emitting Cargo/rustc JSON,
with original-file diagnostic paths. The JSON format is provisional and requires
regeneration/version maintenance. Because the actual compiler already uses Cargo,
this has less favorable effort and maintenance than generated Cargo packages.

### E. Virtual filesystems or custom editors inside VS Code — specialized uses

- `TextDocumentContentProvider` is useful for read-only generated-code views.
- `FileSystemProvider` can expose a curated writable source tree and read-only
  generated files. However, Cargo and rust-analyzer require disk-backed sources;
  the current official extension selects `file:` Rust documents. Custom `idtx:`
  source URIs would need additional filesystem/URI/LSP integration.[R11][R12]
- Custom editor/webview APIs embed a custom UI **inside VS Code**. They do not
  embed VS Code into Conductor, and a webview does not automatically inherit
  native Rust text-editor features.[R13]

Use native file-backed Rust documents for authoring. A custom webview could later
present behavior configuration beside the code if that is valuable.

### F. Other shortcuts and advanced approaches

| Approach | Assessment |
|---|---|
| Install rust-analyzer but continue opening only the loose file | May improve basic syntax handling, but leaves SDK dependencies, target cfgs, macros, and compiler context unresolved. |
| Open the existing cache project and edit its copied source | Very quick demonstration, but saves/refactors target generated copies and preparation overwrites them. |
| Symlink author sources into the scratch crate | Avoids copies, but adds Windows setup, duplicate URI, watcher, and path-resolution issues. The tested direct module reference is simpler. |
| Continuously synchronize editable shadow copies | Requires bidirectional edits, conflict handling, unsaved-buffer mapping, diagnostic mapping, and rename mapping. Disproportionate effort. |
| Point `[lib].path` directly at the user file | Simple Cargo structure, but the current source relies on the outer `#![no_std]` wrapper. A crate-root source contract would need an authored/validated prologue and a deliberate format change. |
| User-managed standalone Cargo crate | Useful expert escape hatch if authors deliberately need full crate ownership; conflicts with the normal managed-build workflow. |
| WSL / SSH / Dev Containers | Useful for a future standardized remote toolchain. Requires the source, SDK, Cargo, and analyzer to agree on the remote filesystem; does not solve the local context problem by itself. |
| Use a VS Code extension as a language-service bridge to Conductor | Provider commands can obtain completions/hover/definitions,[R10] but require a running VS Code window, document synchronization, and IPC. Direct rust-analyzer LSP is a cleaner backend for an embedded editor. |

## 7. Multi-file source organization and user flow

### Ordinary modules first

For new behavior folders, the author writes:

```rust
use idtx_behavior_sdk::prelude::*;

mod motion;                         // motion.rs; can have motion/easing.rs
#[path = "../shared/math.rs"]
mod shared_math;                    // shared source within the source collection

// State and #[behavior] implementation follow.
```

Use the native VS Code Explorer to create files initially. Document the `mod`
declaration and test rust-analyzer's create-module assist. A Conductor file-tree
implementation or extension is not needed just to split source into files.

Keep one behavior entry per generated crate. Shared helper modules should contain
reusable logic rather than another behavior entry exporting the same WASM symbols.
Cross-project reusable Cargo libraries can be a later managed-dependency feature.

### Source discovery and build changes

1. Model an entry plus its source collection/root(s), rather than only a filename.
   A stage's `behaviors/` tree is a simple collection that permits sibling shared
   code. Explicit additional roots can support project-wide libraries later.
2. Stage supported files recursively, preserving their relative paths and original
   entry filename. Support declared `include_str!` / `include_bytes!` data as well
   as `.rs` files. Keep generated wrappers outside the staged author namespace so
   author files named `lib.rs` or `behavior.rs` do not collide with them.
3. For the first implementation, conservatively snapshot/hash the supported source
   collection. Extra invalidations are preferable to silently missing a helper
   change; precise dependency-based invalidation can follow measurement.
4. Remove stale staged files on deletion and account for changes to every source
   root. Preserve the existing regular-file/symlink handling and project-relative
   portability rules when extending traversal.
5. Give published modules a collision-resistant identity. Today the default name is
   the entry file stem: multiple `spin/behavior.rs` and `move/behavior.rs` entries
   would both publish `behavior.wasm`. Generate `BuildOptions.name` consistently
   from a stable source identity, with a readable prefix; update overlay/cook uses.
6. Discover behavior entry points from their registrations or explicit entry
   convention, not every `.rs`. In particular,
   `crates/idtx-behavior-build/tests/scene_examples_compile.rs` currently treats
   every discovered `.rs` as a complete behavior; it must distinguish helper files
   once examples become folders.
7. Keep IDE data out of project delivery and version control. The asset browser
   already hides dot directories, and `crates/idtx-build/src/publication.rs::generated_dir`
   prunes them during recursive collection. Add coverage for the new `.idtx` tree
   and ignore it in generated project Git configuration.

### User-facing operations

- **Create:** create a named source folder with one entry file, register its
  stage-relative source path, prepare the context, and open the entry in VS Code.
- **Edit:** reuse the project's workspace and focus the original entry. Expose its
  helper/shared folders through Explorer.
- **Attach existing project behavior:** reuse its registered source rather than
  silently duplicating one file. Show which attachments share the implementation.
- **Import external behavior:** select the source folder and entry file; copy the
  source collection and declared shared inputs together. Preserve existing single-file
  import for genuinely standalone sources and report unresolved companions.
- **Duplicate / make local copy:** clone the authored source set and create a new
  identity. This also supplies an editable project-local workflow for built-ins.
- **Save → Check → Preview:** rust-analyzer supplies live analysis and compiler
  checks on save. Conductor preview builds saved sources. The initial UX should
  make that saved-source contract explicit; a companion extension can later
  perform Save All before preview. Automatic hot reload is a separate capability.
- **Rename / remove:** update source registrations and USD paths through Conductor
  operations. Refactoring symbols inside source stays with rust-analyzer. Removing
  an attachment should not delete a source still used by another attachment.

Compiler checking and capability verification serve different purposes. Cargo
finds Rust type/borrow/macro errors; build/link Verify checks WASM and granted host
imports. Scene-dependent behavior still needs preview/runtime validation. Preserve
these distinct feedback mechanisms in the UI.

## 8. Embedded editor evaluation

Every embedded option still needs the managed project context above. An editor
widget alone does not provide semantic Rust completion.

Expose embedded support as a Conductor-only feature in the shared shell, and run
language-server and compiler work asynchronously alongside the rendering loop.

### Option 1: native egui editor + rust-analyzer

**Integration fit:** closest to the current application stack. Start with a native
code-editing component and an asynchronous LSP process owned by the shell.

**Required work:**

- reliable text buffers, tabs, line numbers, selection, undo/redo, search, clipboard,
  indentation, dirty state, save/reload/conflicts, and recovery;
- LSP initialization/configuration, versioned document synchronization, file
  watching, cancellation, progress, restart/shutdown, and diagnostic handling;
- completion popups, snippet/additional-edit application, hover, signature help,
  definition navigation, rename/workspace edits, and rustfmt integration;
- UTF-8/UTF-16 position conversion, Unicode/IME behavior, keyboard focus,
  accessibility, and responsiveness alongside the 3D viewport.

Syntax highlighting through egui or a parser is a small part of this work. The
language-server client and high-quality editing interactions dominate. Expect
approximately **4–8 weeks for a useful MVP**, **8–16 weeks for a rounded desktop
experience**; matching VS Code's full editing ecosystem is larger still.

### Option 2: Monaco in a native WebView + rust-analyzer

**Integration fit:** reuses a mature editing component for multi-cursor editing,
undo, folding, decorations, and completion UI. Monaco is the editor component
derived from VS Code; ordinary VS Code extensions do not run in it.[R14]

Suggested architecture:

```text
Monaco + LSP client in WebView
          ↕ local message transport
Conductor editor service / filesystem + process bridge
          ↕ LSP over stdio
rust-analyzer → managed Cargo context → local toolchain
```

Use a maintained adapter such as `monaco-languageclient` rather than implementing
all language-feature adapters individually.[R15] Give models canonical file URIs
matching the backend sources, and use the same source/document model for file
creation, saving, and multi-file workspace edits.

Conductor would additionally need:

- bundled HTML/JS/CSS and worker assets, a frontend build, and offline asset loading;
- a document/file service and LSP transport bridge; Cargo and the native analyzer
  run in the Rust host, not in a browser worker;
- native WebView lifecycle, docking bounds, DPI, input focus, IME, clipboard,
  popups, and layering with egui/wgpu;
- packaging for WKWebView on macOS, WebView2 on Windows, and WebKitGTK on Linux.
  Wry supports child views on macOS/Windows/X11; Wayland requires additional GTK
  integration rather than assuming the same winit child-view path.[R16]

Node is a frontend build-time dependency, not necessarily an end-user prerequisite.
Use compatible, tested Monaco/LSP-adapter versions. Provision the analyzer through
a supported installation or bundle; do not depend on a hard-coded path inside a
user's changing VS Code extension installation.

Estimate **4–7 weeks for an MVP**, **8–14 weeks for a rounded macOS/Windows
integration**, with Linux/Wayland potentially extending the range. If embedding
becomes a firm requirement, this is the preferred candidate to spike because it
reuses more editor behavior than a new native component. A separate tool window
is an easier first WebView prototype than a fully docked panel.

### Option 3: full VS Code / Code-OSS web workbench

Desktop VS Code does not provide a supported editor-as-widget API for an egui
window. OS-window reparenting would be a fragile platform-specific workaround.
VS Code's custom-editor APIs operate in the opposite direction: custom UI hosted
by VS Code.

A Code-OSS/OpenVSCode Server-style workbench can supply a web IDE, but adds a full
IDE backend, extension hosting, process/service lifecycle, packaging, storage, and
update integration. It is a substantially larger product decision than embedding
Monaco. `vscode.dev` alone cannot run local Cargo and native rust-analyzer; it needs
an appropriate backend/remote environment for that workflow.[R17]

Plan on **multiple engineer-months** for a supported integrated product. With VS
Code already a prerequisite, a companion extension offers a much better initial
effort-to-benefit ratio.

## 9. Delivery plan and implementation locations

Effort below is incremental; phases 0–2 form the recommended initial delivery.

| Phase | Deliverable | Approximate effort |
|---|---|---|
| 0 — close the feasibility/compatibility questions | Reproduce completion in actual VS Code; Windows checks; source-format decision; representative export/link parity; verify workspace settings against supported extension versions | 1–3 days |
| 1 — useful VS Code integration | Shared preparation context, module-format path, generated workspace, profile/tooling setup, real-source navigation/diagnostics, stable analysis targets, migration/repair UX | 4–7 days |
| 2 — complete source-folder workflow | Recursive source sets and fingerprinting, artifact identities, create/import/duplicate flow, shared modules, examples and documentation | 5–10 days |
| 3 — optional companion extension | Native source tree/commands/status, shipped helper integration, Save All + session-routed Verify/Preview | 5–15 days, depending on IPC scope |
| 4 — optional embedded-editor spike | One editor window, real SDK completion, diagnostics and save, then native/WebView integration decision | 3–5 days for the spike; implementation ranges in section 8 |

Keep module-wrapper compatibility as an explicit phase-0 decision. If zero-change
support for arbitrary legacy crate-root sources is mandatory, budget the JSON-graph
fallback separately: roughly **2–4 weeks**, including dependency/macro resolution
and diagnostic integration.

### Main code changes

| Location | Planned responsibility |
|---|---|
| `crates/idtx-behavior-build/src/lib.rs` | Reusable development/build context, source-format selection, source-set identity and public preparation API |
| `crates/idtx-behavior-build/src/skeleton.rs`, `skeleton/` | Shared module wrapper, original filenames, recursive snapshotting, source templates |
| `crates/idtx-behavior-build/src/fingerprint.rs`, `compile.rs` | Source-set hashing, settings parity, stable analysis output separation, structured diagnostic access |
| `crates/idtx-behavior-build/src/sdk_sysroot.rs` | Reuse packaged SDK/lock discovery; expose sufficient context for IDE setup |
| `crates/idtx-studio-shell/src/behavior_tools.rs` | Prepare-and-open workspace, editor launch arguments/results, folder-aware authoring operations |
| `crates/idtx-studio-shell/src/actions.rs`, `model.rs`, `render.rs`; `crates/idtx-editor-ui/src/inspector/panels/behavior.rs` | Preparation/setup/repair states, source-folder operations, actionable source errors |
| `crates/idtx-project/src/lib.rs` | Persist authoring-format/source-root metadata as needed, separately from runtime limits |
| `crates/idtx-studio-shell/src/behavior_prepare.rs` and build callers | Carry the same source context and collision-resistant artifact names into preview/cook |
| `tools/xtask/src/main.rs` | Keep repository CLI behavior builds aligned with the selected source format |
| `crates/idtx-behavior-build/tests/`; example and built-in behaviors | Compiler/cache/ABI regressions and realistic multi-file examples |
| Optional new VS Code extension + small headless helper | Commands and UI integration over the existing Rust services |

Keep the preparation library independent of VS Code, egui, and USD. The shell
resolves authored assets and supplies source/build inputs; adapters generate editor
configuration or drive an LSP client. This makes the VS Code investment reusable
for a later embedded editor.

## 10. Acceptance criteria

The following checks test the developer workflow and compiler contract:

1. **Create → edit:** a new behavior opens with its source tree, correctly selected
   rust-analyzer profile, SDK completion, and a visible preparation/indexing state.
2. **Unsaved code:** completion works after `ctx.` inside `#[behavior]`; hover and
   navigation resolve SDK APIs; syntax diagnostics reflect buffer edits.
3. **Compiler diagnostics:** a saved type/borrow error in the entry or helper appears
   in the original file in VS Code. Fixing it clears the diagnostic. Broken behavior
   code does not prevent navigation to otherwise available SDK symbols.
4. **Refactoring:** rename/reference search and formatting edit original author
   files, including helpers, rather than generated snapshots.
5. **Modules:** sibling modules, nested modules, shared source, and declared
   compile-time data work in IDE Check, Conductor Check/Verify, preview, and cook.
6. **Cache correctness:** changing a nested/shared file, preserving its mtime,
   deleting it, or changing the SDK invalidates the correct build. Existing
   artifact-receipt and dynamic-input checks continue to work.
7. **Managed structure:** generated manifests/wrappers and SDK files are read-only
   in the intended workspace; altering IDE scaffolding cannot change the official
   build recipe. Regeneration repairs generated state while preserving author files.
8. **Compatibility:** migrated and legacy source formats use their intended module
   scope and produce valid ABI exports/capability links. Project moves preserve
   USD source resolution after context regeneration.
9. **Multiple behaviors/projects:** identical entry filenames do not overwrite
   artifacts; shared attachments use one source context; repeated editing opens the
   intended project; checks can coexist with preview builds.
10. **Distribution:** packaged Conductor works without the Studio checkout on
    macOS and Windows, including GUI-launch tool discovery, spaces/Unicode in paths,
    and an initially empty IDTX VS Code profile. Generated IDE data is excluded from
    cooked output. Add Linux-specific checks where that authoring platform is supported.

Distinguish fast rust-analyzer feedback on unsaved buffers from compiler checks of
saved files. Full WASM code generation, linking, and runtime behavior require the
existing Verify/Preview path even after the language tooling is integrated.

## References

Primary documentation consulted for this plan:

- **[R1]** [VS Code CLI: profiles, extensions, windows, environment, and URLs](https://code.visualstudio.com/docs/configure/command-line)
- **[R2]** [VS Code profiles](https://code.visualstudio.com/docs/configure/profiles)
- **[R3]** [VS Code multi-root workspaces, settings, tasks, and recommendations](https://code.visualstudio.com/docs/editing/workspaces/multi-root-workspaces)
- **[R4]** [VS Code read-only settings implementation](https://github.com/microsoft/vscode/blob/main/src/vs/workbench/contrib/files/browser/files.contribution.ts)
- **[R5]** [rust-analyzer configuration](https://rust-analyzer.github.io/book/configuration.html)
- **[R6]** [rust-analyzer non-Cargo projects and flycheck](https://rust-analyzer.github.io/book/non_cargo_based_projects.html)
- **[R7]** [rust-analyzer installation and Rust sources](https://rust-analyzer.github.io/book/installation.html)
- **[R8]** [Rust module and `#[path]` resolution](https://doc.rust-lang.org/reference/items/modules.html)
- **[R9]** [VS Code Tree View API](https://code.visualstudio.com/api/extension-guides/tree-view) and [Task Provider API](https://code.visualstudio.com/api/extension-guides/task-provider)
- **[R10]** [VS Code commands, including language-feature provider commands](https://code.visualstudio.com/api/references/commands)
- **[R11]** [VS Code virtual documents](https://code.visualstudio.com/api/extension-guides/virtual-documents) and [virtual workspaces](https://code.visualstudio.com/api/extension-guides/virtual-workspaces)
- **[R12]** [rust-analyzer VS Code client document selector](https://github.com/rust-lang/rust-analyzer/blob/master/editors/code/src/client.ts)
- **[R13]** [VS Code Custom Editor API](https://code.visualstudio.com/api/extension-guides/custom-editors)
- **[R14]** [Monaco Editor concepts and extension limitations](https://github.com/microsoft/monaco-editor)
- **[R15]** [Monaco Language Client and compatibility guidance](https://github.com/TypeFox/monaco-languageclient)
- **[R16]** [Wry: native WebViews, winit integration, and platform constraints](https://github.com/tauri-apps/wry)
- **[R17]** [VS Code for the Web capabilities and limitations](https://code.visualstudio.com/docs/remote/vscode-web)
