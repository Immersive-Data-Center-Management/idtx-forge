# Authoring behaviors in VS Code

Conductor's **Create Behavior** and **Edit Code** open a persistent, managed VS
Code workspace. Rust-analyzer analyzes your original files, including unsaved
buffers, with the same SDK, Rust 2021 edition, WASM target, procedural macros and
release settings used by the behavior compiler.

## Setup and workflow

Install VS Code and a Rust toolchain, then install the authoring components:

```sh
rustup target add wasm32-unknown-unknown
rustup component add rust-src rustfmt
```

1. Create or attach a behavior in the Behavior inspector, or choose **Edit Code**.
   Workspace preparation runs in the background; the inspector shows progress,
   setup errors, and the resolved SDK/toolchain.
2. Conductor opens the **IDTX Behavior** VS Code profile. If it is new/empty,
   choose **Set up rust-analyzer** in the inspector. This installs
   `rust-lang.rust-analyzer` in that profile. A rustup `rust-analyzer` component is
   not required. Trust the workspace in VS Code to enable Cargo and macro execution.
3. After indexing, use completion (including `ctx.` inside `#[behavior]`), hover,
   go-to-definition, rename, formatting, and live syntax diagnostics normally.
   Compiler type/borrow errors appear on save, at the original source locations.
4. **Save all files** before Conductor's **Cargo Check**, **Verify capabilities**,
   or **Preview**. These operations compile saved files. Verify additionally
   builds WASM and checks its imports against the attachment's granted capabilities.

The workspace can remain open with Conductor closed. Opening another behavior in
the project adds its entry to the same workspace; multiple attachments of one
source share one Cargo context. VS Code's matching-workspace routing is used
instead of forcing a new window or targeting the last active window.

**Regenerate Editor Workspace** repairs generated files and refreshes paths after
a project or SDK move. The inspector's ready-state tooltip shows the workspace,
SDK, toolchain and selected VS Code profile.

## Source folders and modules

New behaviors have a named folder:

```text
levels/main/
  scene.usda
  behaviors/
    spin/
      behavior.rs
      motion.rs
      motion/easing.rs
      speed.txt
    shared/
      math.rs
```

The entry is an ordinary Rust **module body**:

```rust
use idtx_behavior_sdk::prelude::*;

mod motion;
#[path = "../shared/math.rs"]
mod math;

const SPEED: &str = include_str!("speed.txt");

#[derive(Default)]
pub struct Spin;

#[behavior(kind = "runtime")]
impl Spin {
    fn on_update(&mut self, ctx: &Ctx, dt: f32) -> Result<()> {
        let _ = (ctx.scene(), dt, SPEED);
        Ok(())
    }
}
```

Use VS Code's Explorer or Rust's create-module assist for new helpers. Helpers do
not need a separate manifest or another `#[behavior]` entry. See
[`examples/behaviors/modules/`](examples/behaviors/modules/) for a compilable
multi-file example.

The default source collection is the nearest enclosing `behaviors/` directory;
otherwise it is the entry's parent directory. Modules and source-relative
`include!`, `include_str!`, and `include_bytes!` inputs must stay in that
collection. Regular files are snapshotted recursively, preserving filenames and
relative paths. Dot entries, `target`, `build`, `dist`, and compiled `.wasm` files
are excluded; symlink inputs are rejected. Deleted helpers are removed from
subsequent snapshots. Every supported source/data file participates in the cache
identity, including changes that preserve file modification times.

- **Attach an in-project entry:** reuse that source; edits apply to every
  attachment referencing it.
- **Import an external entry:** select its `.rs` entry, then the source collection
  containing its helpers/shared inputs. The collection is copied into a new
  level-local folder. A standalone file can use its containing folder.
- **Make Local Copy:** copy the containing source collection and retain the
  attachment's configuration. This also provides editable copies of inherited
  and built-in behaviors. Source references stay relative to the authoring layer.
- **Detach:** remove the attachment; retain source files for other attachments.

### Module-format compatibility and compile-time paths

Both editor analysis and production builds now use:

```rust
#![no_std]
#[path = ".../behavior.rs"]
mod behavior;
```

The editor points directly at the original entry; production points into an
immutable-for-that-build snapshot. Existing flat entry files can keep their
locations. Older sources that used `crate::MyType` for their own types must use
`self::MyType` (or appropriate `super::` paths from helpers): `crate::` now refers
to the managed wrapper. `module_path!()` also includes `behavior`.

`CARGO_MANIFEST_DIR` is the managed Cargo crate, not the author's source folder.
Prefer source-relative include paths. `file!()` describes the compilation input:
an original path in the editor, a staged path in production; do not rely on those
strings being identical. Conductor remaps rendered compiler errors back to the
original author paths without altering the copied lines/columns.

## Managed editor data

```text
project/.idtx/behavior-ide/
  behaviors.code-workspace
  sources.json
  crates/<source-key>/Cargo.toml
  crates/<source-key>/Cargo.lock
  crates/<source-key>/src/lib.rs
  target-ra/
```

Only source collections appear as Explorer roots. Generated files and the selected
SDK/ABI/macro sources are read-only in the workspace, while SDK navigation remains
available. Cargo lockfiles are resolved before enabling `--locked`. Analysis
targets are separate from production builds. Generated **Check** tasks use the
same release/WASM settings and VS Code's Rust problem matcher.

Conductor regenerates its production recipe independently of IDE manifests. Use
the Behavior inspector for runtime configuration. WASM output names contain a
readable source prefix plus a location-derived identity, so two entries named
`behavior.rs` cannot overwrite one another. Generated `.idtx/` and behavior-cache
trees are ignored by Git in new projects and excluded from cooked output.

For repository builds:

```sh
cargo xtask build-behavior path/to/behavior.rs --out path/to/output
# For a custom collection layout or an explicitly named output:
cargo xtask build-behavior path/to/entry.rs --source-root path/to/sources --name demo
```

## Editor overrides

| Environment variable | Meaning |
|---|---|
| `IDTX_BEHAVIOR_SDK` | Selected SDK crate directory; otherwise use Conductor's SDK discovery, including the packaged SDK. |
| `IDTX_BEHAVIOR_VSCODE` | VS Code CLI executable path (including `code.cmd` on Windows). |
| `IDTX_BEHAVIOR_VSCODE_PROFILE` | Existing/custom profile name. An empty value uses VS Code's default profile. Default: `IDTX Behavior`. |
| `IDTX_BEHAVIOR_EDITOR` | Custom editor command. Supports `{workspace}` and `{file}`. |

Prefer a JSON argument array for a custom editor, especially on Windows:

```json
["/path with spaces/code", "--profile", "My Profile", "{workspace}", "--goto", "{file}:1:1"]
```

Quoted legacy command templates are also accepted. Arguments are parsed before
path substitution; if `{file}` is absent, the entry path is appended. Include
`{workspace}` to give a custom editor the managed Cargo context.

### Language-server acceptance probe (developers)

With the desktop build prerequisites configured, run the optional real LSP test:

```sh
IDTX_TEST_RUST_ANALYZER=/path/to/rust-analyzer \
  cargo test -p idtx-studio-shell --features cooking --lib rust_analyzer_completes -- --ignored
```

It uses the generated workspace settings and checks unsaved `ctx.` / `ctx.sc`
completion, SDK hover, helper navigation/rename, formatting, and saved-file type
diagnostics and their removal. The VS Code extension's bundled server can be used.
