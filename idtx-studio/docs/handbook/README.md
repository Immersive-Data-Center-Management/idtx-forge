# IDTX Experience Developer Handbook

This book is for developers using the **packaged IDTX Conductor application**. Read [Start here](src/introduction.md), browse the [table of contents](src/SUMMARY.md), or open the [complete Signal Room example](src/example/index.md). You do not need the IDTX Studio source tree.

The downloadable book directory contains the Markdown chapters, images, tools and a ready-to-copy [Signal Room project](examples/signal-room/README.md). The [complete file listings](src/example/index.md) also let you reproduce that project from a Markdown or HTML edition. The example uses local USD geometry and single-file Rust behaviors, with no engine build helper or external service.

You need a compatible Conductor distribution containing its behavior SDK and the Runtime for your chosen target. Install Rust/Cargo and the WASM target separately; see [setup](src/setup.md). Conductor handles project opening, behavior checks, preview, cooking and packaging. The finished experience runs without Rust or the SDK.

**Always start behavior editing in Conductor’s Inspector > Behavior panel:** choose **Create Behavior…** for a new behavior or **Edit Code** to edit an existing behavior. These actions generate or refresh the Cargo and VS Code workspace that embeds the authored Rust file with the matching SDK, WASM target and toolchain settings. Open the source through this workspace so **rust-analyzer provides code completion, navigation, syntax/type diagnostics and checks on save**. Opening a loose `.rs` file does not establish that context. Follow the [Inspector authoring workflow](src/tutorial/03-behaviors.md#start-every-edit-from-the-inspector), including **Set up rust-analyzer** when prompted.

For a coding agent, provide the [experience authoring contract](src/reference/agent-guide.md), the [SDK reference](src/reference/behavior-sdk.md) and the project’s included [AGENTS.md](examples/signal-room/AGENTS.md). These describe the available APIs, composition rules, capabilities and validation boundaries without repository links.

Try the [shared Rust module exercise](src/tutorial/07-shared-modules.md): two behavior entries import one helper in the managed workspace. It includes [ready-to-copy companion files](examples/shared-modules/README.md), Inspector setup, rust-analyzer navigation and a shared change you can verify in both behaviors.

Behavior authoring guidance was updated on **26 November 2026**, following the SDK/API review on **23 November 2026**. The update covers managed editor workspaces, rust-analyzer setup and repair, helper modules, and shared-source/local-copy authoring. Project schema: **1**; runtime manifest schema: **2**; behavior ABI: **3**. Use a Conductor, SDK and Runtime distribution from a compatible release. Read the [compatibility map](src/appendices/implementation-map.md) and [validation record](src/appendices/validation.md) for the practical limits of the examples.

The current API refresh adds composed object bindings, curve-distance queries, pointer identity/modal capture, panel-background clicks, radial/polygon widgets, and named animation clips with status and completion events. Follow the [animation and routing exercise](src/tutorial/06-animation-and-routing.md) and [widget extension](src/reference/widgets.md) after Signal Room. The [agent guide](src/reference/agent-guide.md) includes the same current contracts.

## Read or export the book

Every page is ordinary Markdown. A Mermaid-enabled Markdown viewer renders the diagrams. To build optional HTML, install mdBook, open a terminal in **this handbook directory**, and run:


```sh
mdbook build
mdbook serve --open
```

For Mermaid diagrams in HTML, install mdbook-mermaid and run `mdbook-mermaid install .` before building. Without that optional integration, the diagrams remain readable Mermaid source. Generated HTML goes into `book/`. Keep the complete book directory when sharing the Markdown edition; its links and assets are contained here.

Python 3.11 or later is optional for checking the book and reconstructing the example:

```sh
python tools/check_book.py
python tools/create_example.py --check
python tools/create_example.py ../SignalRoom
```

The example extractor refuses to overwrite existing authored files. Copying the companion project or saving the complete listings requires no Python. Follow the [capture guide](src/appendices/screenshots.md) for additional Conductor or device screenshots.
