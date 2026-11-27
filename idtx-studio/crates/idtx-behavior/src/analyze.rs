//! Static capability analysis of a compiled behavior module.
//!
//! Answers two questions an authoring tool needs *before* a stage is ever
//! loaded in the runtime:
//!
//! 1. **Would this behavior link** given the capabilities authored on its prim?
//!    ([`verify_link`]) — this reproduces the exact runtime gate, because it
//!    *is* [`crate::linker::build_linker`] + [`wasmtime::Linker::instantiate_pre`],
//!    the same path [`crate::CompiledModule::instantiate`] takes at attach time.
//! 2. **Which capabilities does the code actually require?**
//!    ([`required_capabilities`]) — the minimal grant that still links.
//!
//! The required set is derived by *probing [`build_linker`] itself* rather than
//! by maintaining a second import→capability table: start from
//! [`Capabilities::all`] and greedily drop each bucket that the module still
//! links without. This cannot drift from the real linker, since the linker is
//! the only authority consulted.
//!
//! None of this runs guest code — [`instantiate_pre`](wasmtime::Linker::instantiate_pre)
//! only type-checks imports, so there is no store, no fuel, and no epoch budget.
//!
//! Note on the `net` bucket: its host imports are additionally behind the crate
//! `net` feature ([`build_linker`]). A build without that feature does not link
//! the HTTP imports even when `net` is granted, so a behavior that calls them is
//! reported as having unsatisfiable imports here. Verify with the `net` feature
//! enabled to match a net-capable runtime.

use wasmtime::{Engine, Module};

use crate::linker::build_linker;
use crate::manifest::{Capabilities, KNOWN_CAPABILITIES};

/// Error surfaced while analysing a module.
#[derive(Debug, thiserror::Error)]
pub enum AnalyzeError {
    /// wasmtime failed to compile/validate the module.
    #[error("wasmtime error: {0}")]
    Wasm(#[from] wasmtime::Error),
    /// The module references imports that no capability grant can satisfy —
    /// a non-`idtx` import, an unknown `idtx` name, or a signature mismatch.
    /// Carries the wasmtime linker message naming the offending import.
    #[error("module references imports no capability can satisfy: {0}")]
    UnsatisfiableImports(String),
}

/// The result of checking a module against an authored capability grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkReport {
    /// Whether the module links under `configured` — the authoritative runtime
    /// gate. Equivalent to `missing.is_empty()`.
    pub links: bool,
    /// The minimal capability set the module actually needs to link.
    pub required: Capabilities,
    /// Required but not granted (`required & !configured`) — these cause the
    /// `LinkFailed` at stage load.
    pub missing: Capabilities,
    /// Granted but unused (`configured & !required`) — safe to drop.
    pub unused: Capabilities,
}

/// The `"idtx"`-namespaced imports the compiled `wasm` references.
///
/// Mirrors how [`crate::ExportedHooks`] walks `module.exports()`, but for the
/// import side.
pub fn idtx_imports(wasm: &[u8]) -> Result<Vec<String>, AnalyzeError> {
    let engine = Engine::default();
    let module = Module::new(&engine, wasm)?;
    Ok(module
        .imports()
        .filter(|i| i.module() == "idtx")
        .map(|i| i.name().to_owned())
        .collect())
}

/// The minimal capability grant under which `wasm` still links.
pub fn required_capabilities(wasm: &[u8]) -> Result<Capabilities, AnalyzeError> {
    let engine = Engine::default();
    let module = Module::new(&engine, wasm)?;
    required_caps_impl(&engine, &module)
}

/// Check `wasm` against the `configured` capability grant, reporting whether it
/// links plus the required / missing / unused sets.
pub fn verify_link(wasm: &[u8], configured: Capabilities) -> Result<LinkReport, AnalyzeError> {
    let engine = Engine::default();
    let module = Module::new(&engine, wasm)?;
    let required = required_caps_impl(&engine, &module)?;
    let links = link_resolves(&engine, &module, configured)?;
    Ok(LinkReport {
        links,
        required,
        missing: required.difference(configured),
        unused: configured.difference(required),
    })
}

/// `true` if a linker built from `caps` resolves every import of `module`.
/// This is a pure import type-check: no guest code runs.
fn link_resolves(
    engine: &Engine,
    module: &Module,
    caps: Capabilities,
) -> Result<bool, wasmtime::Error> {
    let linker = build_linker(engine, caps)?;
    Ok(linker.instantiate_pre(module).is_ok())
}

/// Greedy minimal-set probe (see module docs).
fn required_caps_impl(engine: &Engine, module: &Module) -> Result<Capabilities, AnalyzeError> {
    // With every bucket granted the module must link; if it does not, it
    // references something no capability provides — surface wasmtime's message
    // (it names the offending import).
    let full = build_linker(engine, Capabilities::all())?;
    if let Err(e) = full.instantiate_pre(module) {
        return Err(AnalyzeError::UnsatisfiableImports(e.to_string()));
    }

    let mut caps = Capabilities::all();
    for &token in KNOWN_CAPABILITIES {
        let mut trial = caps;
        trial.set_token(token, false);
        if link_resolves(engine, module, trial)? {
            caps = trial;
        }
    }
    Ok(caps)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tiny guest module (WAT) that imports the given `idtx` functions
    /// and exports the ABI-version probe, so it is a realistic behavior shape.
    fn wasm_importing(imports: &[(&str, &str)]) -> Vec<u8> {
        let mut src = String::from("(module\n");
        for (name, sig) in imports {
            src.push_str(&format!("  (import \"idtx\" \"{name}\" (func {sig}))\n"));
        }
        // _idtx_abi_version export so the module resembles a real behavior.
        src.push_str("  (func (export \"_idtx_abi_version\") (result i32) i32.const 3)\n");
        src.push_str(")\n");
        wat::parse_str(&src).expect("valid wat")
    }

    #[test]
    fn required_caps_match_imports() {
        // idtx_set_xform is gated on `xform_write`; idtx_entity_find on `scene_query`.
        let wasm = wasm_importing(&[
            ("idtx_set_xform", "(param i64 i32) (result i32)"),
            ("idtx_entity_find", "(param i32 i32 i32) (result i32)"),
        ]);
        let required = required_capabilities(&wasm).expect("analyzable");
        assert!(required.xform_write, "set_xform requires xform_write");
        assert!(required.scene_query, "entity_find requires scene_query");
        assert!(!required.events, "no event import → events not required");
        assert!(!required.net, "no net import → net not required");
    }

    #[test]
    fn always_linked_imports_require_nothing() {
        // idtx_log carries no authority and is always linked.
        let wasm = wasm_importing(&[("idtx_log", "(param i32 i32 i32)")]);
        let required = required_capabilities(&wasm).expect("analyzable");
        assert!(required.is_empty(), "log-only behavior needs no capability");
    }

    #[test]
    fn verify_reports_missing_and_unused() {
        let wasm = wasm_importing(&[("idtx_set_xform", "(param i64 i32) (result i32)")]);
        // Grant the wrong bucket: events granted, xform_write withheld.
        let configured = Capabilities::from_tokens(["events"]);
        let report = verify_link(&wasm, configured).expect("analyzable");
        assert!(!report.links, "xform_write ungranted → will not link");
        assert!(report.missing.xform_write, "xform_write is missing");
        assert!(report.unused.events, "events is granted but unused");
    }

    #[test]
    fn verify_links_with_exact_grant() {
        let wasm = wasm_importing(&[("idtx_set_xform", "(param i64 i32) (result i32)")]);
        let configured = Capabilities::from_tokens(["xform_write"]);
        let report = verify_link(&wasm, configured).expect("analyzable");
        assert!(report.links, "exact grant links");
        assert!(report.missing.is_empty());
        assert!(report.unused.is_empty());
    }

    #[test]
    fn foreign_import_is_unsatisfiable() {
        // An import outside the "idtx" namespace can never be satisfied.
        let wasm = {
            let src = "(module (import \"env\" \"malloc\" (func (param i32) (result i32))))";
            wat::parse_str(src).expect("valid wat")
        };
        let err = required_capabilities(&wasm).expect_err("foreign import");
        assert!(matches!(err, AnalyzeError::UnsatisfiableImports(_)));
    }
}
