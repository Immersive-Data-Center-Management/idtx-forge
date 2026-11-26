# 2. Prepare the development environment

Start with a complete Conductor authoring distribution for your desktop. Install or unpack it into its own directory, and keep all bundled libraries and resources together. Put your experience project in a separate writable directory. The local Signal Room example needs no network service, native USD development installation or engine compilation.

## Required tools and distribution contents

| Requirement | Purpose |
|---|---|
| Packaged IDTX Conductor | Project authoring, source behavior checks, preview and Build controls |
| Matching bundled behavior SDK | Compiler inputs and the public guest API |
| Matching supplied desktop Runtime | Desktop preview and finished desktop packages |
| Rust stable with Cargo, at least 1.90 for this SDK edition | Compile Rust behavior sources |
| Rust target `wasm32-unknown-unknown` | Compile behavior guests to WASM |
| Rust components `rust-src` and `rustfmt` | Standard-library analysis and formatting in the managed behavior workspace |
| Graphics adapter and current driver | Authoring viewport and Runtime rendering |
| VS Code with its `code` command and rust-analyzer | Behavior completion, navigation and diagnostics in the workspace opened by the Inspector; Conductor can install rust-analyzer in its selected profile |
| Text editor | Edit USD and TOML files |

Install Rust through the [official rustup installation instructions](https://www.rust-lang.org/tools/install), including the host prerequisites it requests. On Windows, Rust’s MSVC host tools may require the Visual Studio linker/build tools for native procedural macros. This is a Rust toolchain prerequisite; you do not build OpenUSD or the IDTX engine. Then run these commands in a terminal that can also launch Conductor:

```sh
rustc --version
cargo --version
rustup target add wasm32-unknown-unknown
rustup component add rust-src rustfmt
```

If Conductor was open during installation, restart it so it inherits the updated PATH. Cargo can need an initial download of Rust crate dependencies; later builds can use the local cache. Rust compilation errors and unavailable crate downloads are different failures. The authoring distribution must make its SDK available; installing Rust alone does not supply IDTX APIs.

Install [Visual Studio Code](https://code.visualstudio.com/) and make its `code` command available. Conductor opens behavior workspaces in the **IDTX Behavior** VS Code profile by default. An extension installed in another profile is not necessarily available there; use the Inspector’s **Set up rust-analyzer** action if prompted. Trust your project’s generated workspace in VS Code so Cargo and SDK procedural macros can run.

## Locate the packaged SDK and Runtime

The behavior SDK is a small Cargo workspace with its own Cargo.toml and Cargo.lock, plus the SDK, ABI and macro crates. Conductor locates it from its distribution:

```text
Windows:
  <Conductor-folder>/behavior-sdk/idtx-behavior-sdk/Cargo.toml

macOS:
  Conductor.app/Contents/Resources/behavior-sdk/idtx-behavior-sdk/Cargo.toml
```

Internal crate dependencies are relative within that workspace. Keep it intact. You do not create a Cargo project for the example. The Inspector’s **Create Behavior…** and **Edit Code** actions generate the Cargo context and VS Code workspace under the experience project’s `.idtx/behavior-ide/`, referencing the authored source and bundled SDK. The example remains ordinary Rust source without an author-maintained `Cargo.toml`.

The desktop Runtime normally sits beside Conductor in the Windows distribution, or as a supplied sibling app/executable in the macOS distribution. The complete package also carries native libraries, USD plugins/schema resources and built-in player assets. Treat the distributor’s layout as a unit; moving only the executable can break startup, preview or cooking.

If the SDK directory or target Runtime is absent, obtain a complete compatible authoring distribution from your supplier. A payload-only cook result means the target player was not packaged. The [compatibility map](appendices/implementation-map.md) explains what each supplied component enables. [Target preparation](delivery/targets.md) covers Android SDK/JDK and Apple tools needed later for headset packaging.

## Create a writable example project

The [complete example](example/index.md) offers three equivalent choices: copy the supplied project directory, reconstruct it with the optional Python extractor, or save the full Markdown listings. Choose a **new** destination rather than merging over existing authoring work.

Open a terminal in the **handbook directory**. On Windows:

```powershell
Copy-Item -LiteralPath "examples/signal-room" -Destination "../SignalRoom" -Recurse
```

On macOS/Linux:

```sh
cp -R examples/signal-room ../SignalRoom
```

Python 3.11 or later can reconstruct the same files directly from the book listings:

```sh
python tools/create_example.py ../SignalRoom
```

After copying or extracting, SignalRoom/project.toml is the project manifest. Keep all the example’s USD, Rust and TOML files together. The input map preserves the supplied player’s actions as well as the widget click action.

## Open it in Conductor

Launch the installed Conductor app and choose **Project > Open Project…**, then select SignalRoom/project.toml. You can also launch with an explicit project path:

```powershell
& "C:/IDTX/Conductor/idtx-conductor.exe" --project "C:/IDTX/Projects/SignalRoom/project.toml"
```

Adjust the app and project paths to your installation. On macOS, open Conductor.app and select the project, or invoke Contents/MacOS/idtx-conductor with the same --project argument.

Hub should open with a console and blue beacon. Select the Console in the Stage tree and expand **Inspector > Behavior**. **Always start behavior editing here**, using **Create Behavior…** for a new attachment or **Edit Code** for an existing one. Signal Room already has its attachments, so use **Edit Code**. If the controls are disabled because the binding is inherited, open `assets/station.usda` in Conductor and select its Console to edit the shared implementation. See [shared behaviors](tutorial/03-behaviors.md#share-or-customize-an-implementation) for deliberate local variations.

Wait for workspace preparation to finish. Conductor opens `.idtx/behavior-ide/behaviors.code-workspace` in VS Code and selects the original source file. If requested, click **Set up rust-analyzer** in the Inspector. Confirm SDK completion and Rust diagnostics are available in that workspace before editing; opening the `.rs` file on its own does not configure its generated crate. The [behavior walkthrough](tutorial/03-behaviors.md#start-every-edit-from-the-inspector) explains the workspace and repair controls.

Save all edited files in VS Code, then run **Cargo Check** and **Verify capabilities** in the Behavior inspector. Repeat the workflow for Pointer. Cargo Check validates the saved source; Verify capabilities compares the binding’s grants with its required operations. Neither runs gameplay. Return to Hub for preview if you opened the station asset to edit its shared bindings.

Save authored changes, then choose **Preview > Preview in Desktop**. Conductor prepares the active level and launches its supplied Runtime with the player composition. Use this to verify the beacon, mouse look, widget clicks and outline. Use **Build > Build Desktop Experience** later for the full Hub/Lab transition registry.

## First checkpoint

Continue when the copied project opens, both levels appear in Project Settings, references resolve, the Inspector opens the managed workspace with rust-analyzer completion and diagnostics, the behavior checks pass and Desktop preview launches. Record your Conductor/SDK/Runtime release and Rust version. You now have all authoring prerequisites for the base project; the finished experience will carry its own Runtime and content.

**Next:** [Set up the project](composition/project.md).
