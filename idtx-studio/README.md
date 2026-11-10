![OpenUSD 26.11](https://img.shields.io/badge/OpenUSD-26.11.1-brightgreen)
![Rust 1.90](https://img.shields.io/badge/Rust-1.90-brightgreen)
![CXX 1.0](https://img.shields.io/badge/cxx-1.0-brightgreen)
![BevyECS 0.19](https://img.shields.io/badge/BeavyEcs-0.19-brightgreen)
![EGui 0.35](https://img.shields.io/badge/egui-0.35-brightgreen)
![WGPU 29](https://img.shields.io/badge/wgpu-29-brightgreen)

# IDTX Studio



## Build


### 1. Install OpenUSD (26.11.1)

Install openUSD with the python install script. It will provide the exact path to the installation that shall be used as value for the
`IDTX_USD_DIR` that is required when building the workspace / idtx-studio apps.

```bash
$ > python idtx-studio/tools/build-scripts/build_openusd.py 

$ > export IDTX_USD_DIR=<path/to/openUsd/26.11.1/install>
```