//! Assembly-stage composition — **re-exported from `idtx-build`**.
//!
//! The composer (planner + executor) is the single source of truth living in
//! [`idtx_build::assembly`]; this module simply re-exports it so the shell's
//! preview/tabs call sites keep their short `crate::assembly::…` paths. The
//! whole surface is gated behind the `cooking` feature (which pulls `idtx-build`
//! and enables `idtx-build/cooking`), so a lean build without cooking — Poser,
//! Inspector — compiles none of it.
//!
//! See [`idtx_build::assembly`] for the `AssemblyLevel` / `AssemblyPlan` /
//! `AssemblyCfg` types, the two planners (`plan_assembly` for preview,
//! `plan_cook_assembly` for cook), and `author_assembly_stage`.

#[cfg(feature = "cooking")]
pub use idtx_build::assembly::*;
