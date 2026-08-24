[![REUSE status](https://api.reuse.software/badge/github.com/Immersive-Data-Center-Management/idtx-forge)](https://api.reuse.software/info/github.com/Immersive-Data-Center-Management/idtx-forge)

# IDTX Forge

## About this project

Immersive Digital Twin Experience - [OpenUSD](https://openusd.org/) layer optimization, annotation and content generation tool. It provides a collection of subcommands that operate on one or more input USD files (`.usd`, `.usda`, `.usdc`, `.usdz`) and write the transformed
results to an output directory.

Typical use cases include stage inspection, mesh triangulation, level-of-detail (LOD) generation,
normal/tangent computation, collision-shape generation, native USD instancing,
extent (re)computation, and unit conversion.

## Quick Start

```bash
# Show top-level help and the list of subcommands
idtx-forge --help

# Show help for a specific subcommand
idtx-forge triangulate --help

# Triangulate a mesh and write the result to ./out
idtx-forge -i mesh.usd -o ./out triangulate --algorithm fan

# Generate 5 LODs with QEM decimation, verbose logging to a file
idtx-forge -v --log run.log -i model.usdz -o ./out reduce --algorithm qem --lods 5

# Validate inputs without writing anything (dry run)
idtx-forge --dry-run -i model.usd collision --algorithm quickhull --complexity high
```

The general invocation pattern is:

```
idtx-forge [global options] <subcommand> [subcommand options]
```

## Requirements and Setup

IDTXForge is built with [SCons](https://scons.org/). The build script
(`SConstruct`) is largely self-bootstrapping: on first invocation it clones and
bootstraps [vcpkg](https://github.com/microsoft/vcpkg), installs the declared
vcpkg dependencies, and then clones and builds [OpenUSD](https://github.com/PixarAnimationStudios/OpenUSD)
from source (monolithic, no Python by default). Because OpenUSD is compiled from
source on the first build, expect the initial build to take a significant amount
of time.

### Requirements

**All platforms:**

- **Python 3** (`python3` / `python`) — required to run SCons and OpenUSD's
  `build_usd.py` build script.
- **SCons** — `pip install scons`.
- **Git** — used to clone vcpkg and the OpenUSD sources.
- **CMake** — required by the OpenUSD build (invoked internally by
  `build_usd.py`).
- A **C++20**-capable compiler toolchain (see per-platform notes).
- Sufficient disk space and time for the initial OpenUSD build.

**Windows:**

- Visual Studio 2022 with the *Desktop development with C++* workload
  (MSVC v14.3 toolset, `x64`). The build locates the toolchain via `vswhere.exe`
  and `vcvars64.bat`, so a standard Visual Studio installation is required.

**macOS:**

- Xcode command-line tools (Clang).

**Linux:**

- GCC or Clang with C++20 support.

### Build Commands

Run all commands from the repository root.

```bash
# Default build (release). Bootstraps OpenUSD on first run,
# compiles idtx-forge, and installs the binary + runtime libs into ./bin
scons

# Debug build
scons target=debug

# Build and run the unit tests (compiles tests/*.cpp into bin/IdtxForge-tests)
scons tests=1
```

Available build arguments:

| Argument | Values | Default | Description |
|----------|--------|---------|-------------|
| `target` | `release`, `debug` | `release` | Optimization / debug-symbol configuration. |
| `tests`  | `0`, `1`          | `0` | When set, additionally builds the test binary. |

The resulting executable and the required runtime libraries (OpenUSD monolithic
library, TBB, and USD plugin configuration) are installed into `./bin`.

---

## Commands (TL;DR)

Each subcommand processes every input file and writes the result as
`<original_stem>_<suffix>.<original_ext>` into the output directory (see the
per-command tables below for the exact suffix). All mutating commands honor the
global `--dry-run` flag.

### `dump` — Inspect stage metadata (read-only)

**TL;DR:** Prints a summary of each input stage. Writes nothing; ignores
`--output-dir` and `--dry-run`.

```bash
idtx-forge -i model.usd dump
idtx-forge -i a.usd b.usdz dump
```

For every input file the following information is reported:

- Root layer metadata: up-axis, meters-per-unit, default prim, and whether the
  stage contains animation data (a non-trivial start/end time range).
- Total prim count.
- Number of prims without triangle faces (mesh prims that are not fully
  triangulated, plus a count of non-mesh prims for context).
- Number of pseudo-instances.
- Number of native instance prototypes and native instances.
- Stage-wide geometry totals across all mesh prims: total points, total
  face-vertex indices, and total triangles.
- File size on disk.
- The largest mesh prim by point count and by face count.

| Option | Values | Description |
|--------|--------|-------------|
| `--include-referenced` | – | Also traverse referenced/payloaded prims. |

Output: none (read-only, prints to log/console).

---

### `triangulate` — Convert polygons to triangles

**TL;DR:** Triangulates every `UsdGeomMesh` in the stage.

```bash
idtx-forge -i mesh.usd -o ./out triangulate --algorithm fan
idtx-forge -i mesh.usd -o ./out triangulate --algorithm beauty
```

| Option | Values | Description |
|--------|--------|-------------|
| `--algorithm` | `fan`, `beauty` | `fan`: simple fan triangulation anchored at the first vertex. `beauty`: angle-optimized ear-clipping that maximizes the minimum interior angle (better quality, requires vertex positions). |
| `--include-referenced` | – | Also process referenced/payloaded prims. |

Output suffix: `_triangulated`

---

### `reduce` — Generate Levels of Detail (LODs)

**TL;DR:** Decimates each mesh once per LOD level, emitting a separate file per
level. For `.usdz` inputs, per-LOD textures are also downsampled unless disabled.

```bash
idtx-forge -i model.usd -o ./out reduce --algorithm qem --lods 5
idtx-forge -i model.usdz -o ./out reduce --algorithm edgecollapse --no-texture-reduction
```

| Option | Values | Description |
|--------|--------|-------------|
| `--algorithm` | `qem`, `edgecollapse` | Decimation algorithm. `qem`: Quadric Error Metrics. `edgecollapse`: edge-collapse simplification. |
| `--lods` | integer (default `3`) | Number of LOD levels to emit, including LOD0. |
| `--no-texture-reduction` | – | Do **not** downsample textures per LOD (`.usdz` only). |
| `--include-referenced` | – | Also process referenced/payloaded prims. |

Output suffix: `_LOD<N>` (one file per level)

---

### `extend` — (Re)compute prim extents

**TL;DR:** Recomputes the bounding extent of every `UsdGeomBoundable` prim using
USD's built-in `ComputeExtentFromPlugins()`.

```bash
idtx-forge -i mesh.usd -o ./out extend --behavior preserve
idtx-forge -i mesh.usd -o ./out extend --behavior overwrite
```

| Option | Values | Description |
|--------|--------|-------------|
| `--behavior` | `preserve` (default), `overwrite` | `preserve`: only author extents that are missing. `overwrite`: always recompute; if a differing extent already exists it is replaced and a warning is logged. |
| `--include-referenced` | – | Also process referenced/payloaded prims. |

Output suffix: `_extend`

---

### `normals` — Compute vertex normals

**TL;DR:** Calculates vertex normals for every `UsdGeomMesh`.

```bash
idtx-forge -i mesh.usd -o ./out normals --algorithm faceweighted
idtx-forge -i mesh.usd -o ./out normals --algorithm angleweighted --behaviour overwrite
```

| Option | Values | Description |
|--------|--------|-------------|
| `--algorithm` | `faceweighted`, `angleweighted` | `faceweighted`: weight each adjacent face by its area. `angleweighted`: weight by interior angle at the shared vertex (less sensitive to tessellation density). |
| `--behaviour` | `preserve` (default), `overwrite` | `preserve`: only fill in missing normals. `overwrite`: recalculate and replace all normals. |
| `--include-referenced` | – | Also process referenced/payloaded prims. |

Output suffix: `_normals`

---

### `tangents` — Compute tangent space

**TL;DR:** Calculates tangents/bitangents for every `UsdGeomMesh`. Requires UV
coordinates and normals to be present.

```bash
idtx-forge -i mesh.usd -o ./out tangents --algorithm mikktspace
idtx-forge -i mesh.usd -o ./out tangents --algorithm gramschmidt
```

| Option | Values | Description |
|--------|--------|-------------|
| `--algorithm` | `mikktspace`, `gramschmidt` | `mikktspace`: industry-standard MikkTSpace (recommended, consistent with most DCC tools/engines). `gramschmidt`: Gram-Schmidt orthogonalization from UV gradients (faster, less robust). |
| `--include-referenced` | – | Also process referenced/payloaded prims. |

Output suffix: `_tangents`

---

### `collision` — Generate collision shapes (Planned - not Implemented, yet)

**TL;DR:** Generates optimized collision geometry for every `UsdGeomMesh`.

```bash
idtx-forge -i mesh.usd -o ./out collision --algorithm primitive --shape box
idtx-forge -i mesh.usd -o ./out collision --algorithm quickhull --complexity high
```

| Option | Values | Description |
|--------|--------|-------------|
| `--algorithm` | `primitive`, `quickhull` | `primitive`: fit a primitive shape around the mesh (cheapest, least accurate). `quickhull`: convex hull via QuickHull (more accurate, still convex). |
| `--shape` | `box` (default), `sphere`, `capsule`, `cylinder` | Primitive shape to fit (only used with `--algorithm primitive`). |
| `--complexity` | `low`, `medium` (default), `high` | Level of detail of the generated collision geometry. |

Output suffix: `_collision`

---

### `instancing` — Convert to native USD instances

**TL;DR:** Converts prims into native USD scenegraph instances to reduce
duplication.

```bash
idtx-forge -i pseudo.usda -o ./out instancing --mode pseudo
idtx-forge -i model.usdz -o ./out instancing --mode identical-mesh
```

| Option | Values | Description |
|--------|--------|-------------|
| `--mode` | `pseudo` (default), `identical-mesh` | `pseudo`: flip `over` prototypes referenced by pseudo-instances to `class` and author `instanceable = true` on them. `identical-mesh`: detect meshes with identical geometry, promote the first to a shared `class` prototype under `/__Prototypes__`, and turn the rest into native instances (per-instance transforms preserved). |
| `--include-referenced` | – | Also consider referenced/payloaded prims. |

Output suffix: `_instanced`

---

### `mpu` — Adjust meters-per-unit

**TL;DR:** Changes the stage's `metersPerUnit` metadata to a new target while
preserving the rendered/physical size by folding a compensating uniform scale
onto the stage's default/root prim.

```bash
idtx-forge -i model.usd -o ./out mpu --target meter
idtx-forge -i model.usd -o ./out mpu --target cm
idtx-forge -i model.usd -o ./out mpu --target 0.01
```

The compensating scale is computed as `currentMPU / targetMPU`, so that for any
authored coordinate `p` the physical size is preserved:

```
(p * scale) * targetMPU == p * currentMPU
```

If the stage has no default prim, a single `Xform` root prim (`/Root`) is
injected, all existing root-level prims are reparented beneath it, and it is set
as the default prim carrying the compensating scale.

| `--target` name | metersPerUnit |
|-----------------|---------------|
| `meter` / `m`   | 1.0           |
| `cm`            | 0.01          |
| `mm`            | 0.001         |
| `feet` / `ft`   | 0.3048        |
| `inch` / `in`   | 0.0254        |
| `<value>`       | raw value, e.g. `0.01` |

Output suffix: `_mpu`

## Support, Feedback, Contributing

This project is open to feature requests/suggestions, bug reports etc. via [GitHub issues](https://github.com/Immersive-Data-Center-Management/idtx-forge/issues). Contribution and feedback are encouraged and always welcome. For more information about how to contribute, the project structure, as well as additional contribution information, see our [Contribution Guidelines](CONTRIBUTING.md).

## Security / Disclosure
If you find any bug that may be a security problem, please follow our instructions at [in our security policy](https://github.com/Immersive-Data-Center-Management/idtx-forge/security/policy) on how to report it. Please do not create GitHub issues for security-related doubts or problems.

## Code of Conduct

We as members, contributors, and leaders pledge to make participation in our community a harassment-free experience for everyone. By participating in this project, you agree to abide by its [Code of Conduct](https://github.com/Immersive-Data-Center-Management/.github/blob/main/CODE_OF_CONDUCT.md) at all times.

## Licensing

Copyright 2026 SAP SE or an SAP affiliate company and idtx-forge contributors. Please see our [LICENSE](LICENSE) for copyright and license information. Detailed information including third-party components and their licensing/copyright information is available [via the REUSE tool](https://api.reuse.software/info/github.com/Immersive-Data-Center-Management/idtx-forge).
