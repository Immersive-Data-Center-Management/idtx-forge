# Capture screenshots for the book

Use screenshots for visible checkpoints and diagrams for composition or callback timing. Caption each image with its app/runtime release, project state and step. The book already includes runtime Hub/Lab captures and a separately captioned widget-rendering illustration. Additional Conductor/device pictures can be supplied later without preventing reproduction from the full file listings.

## Capture Conductor and device views

Use OS window capture while following the installed app workflow. Conductor does not have a documented scripted screenshot tour in this edition.

| Suggested image | Capture state | What it explains |
|---|---|---|
| project-open.png | Signal Room open; project files and level controls visible | Writable project root and registered levels |
| usd-reference.png | Hub and referenced station hierarchy visible | Authored level versus reusable asset |
| behavior-attachment.png | Console selected, Behavior inspector expanded | Source, kind, grants and typed context |
| behavior-check.png | Cargo Check and Verify capabilities results | Compilation versus binding verification |
| player-composition.png | Preview player and composed namespace | Source content combined with desktop/XR player |
| cook-complete.png | Latest successful Build result and output path | Cook target and finished artifact |
| lab-spawn-ready.png | Lab after successful specimen callback | Prepared prototype and terminal readiness |
| device-view.png | Actual Quest or Vision Pro run | Target rendering and real controls |

Use a readable window size and relevant panels. Remove credentials and private details from the capture. Record active level, selected prim and action sequence so the image can be reproduced. Save your own evidence outside authored assets, then copy selected images into this book’s src/images directory when updating the book.

## Optional supplied-Runtime PNG capture

A Runtime supplied with metrics support accepts --screenshot and --frames. Screenshot capture uses the headless rendering path. Check the distributed Runtime’s help before using these options; the app cannot acquire a missing compiled feature from a command-line flag.

For the finished Windows package, open a terminal in its **package directory** and run:

```powershell
& "./idtx-runtime.exe" --help
& "./idtx-runtime.exe" --mode desktop --frames 30 --screenshot "C:/IDTX/Captures/hub-cooked.png"
```

Use the corresponding installed executable name on other supported desktop platforms. Keeping run.toml adjacent and omitting --stage makes the runtime load the delivered entry and level configuration. A direct source-stage invocation would test a different composition. The selected player camera or a fallback determines the view; adding an arbitrary USD Camera alone does not necessarily select it.

This bounded capture checks startup and selected frames. Interactive travel, pointer input, spawn readiness and device performance need their own acceptance sequence. If the supplied Runtime lacks PNG readback, use OS capture and keep the same caption/evidence convention.

## Embed an actual image

From a top-level chapter, use this pattern **after the PNG exists**:

```markdown
![Conductor showing the console behavior attachment](images/behavior-attachment.png)

*Conductor, Signal Room Hub, Console selected after behavior checks.*
```

From a nested page, use ../images/ instead. Add only real image links; use Mermaid diagrams for explanations that do not need UI evidence. The [validation record](validation.md) describes the scope of the images currently in this edition.
