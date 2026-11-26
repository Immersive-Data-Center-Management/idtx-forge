//! Materialise the embedded cargo skeleton into a scratch dir.
//!
//! The skeleton (`crates/idtx-behavior-build/skeleton/`) is embedded via
//! `include_str!` so the compiled `idtx-behavior-build` needs no files on
//! disk. Materialisation writes:
//!
//! ```text
//! <scratch>/
//!   Cargo.toml           (from Cargo.toml.tmpl, placeholders substituted)
//!   src/lib.rs           (the #[path] module wrapper)
//!   src/authored/...     (production snapshot, with original relative paths)
//! ```
//!
//! The behavior's host manifest (capabilities, inputs, kind, uuid, …) is no
//! longer a sidecar `idtx.toml`: it lives on the `IdtxBehaviorAPI` USD schema
//! attributes the Conductor authors onto the behavior's prim.

use std::path::Path;

use crate::Result;

/// Embedded skeleton files.
const CARGO_TMPL: &str = include_str!("../skeleton/Cargo.toml.tmpl");
const LIB_SHIM: &str = include_str!("../skeleton/lib.rs");
const AUTHOR_BEHAVIOR_TMPL: &str = include_str!("../skeleton/author_behavior.rs.tmpl");

/// Materialise the skeleton at `scratch`, substituting the crate name +
/// SDK path into `Cargo.toml`. The wrapper references `entry` as a normal Rust
/// module: a live author file for development, or a staged file for production.
pub fn materialise(
    scratch: &Path,
    crate_name: &str,
    sdk_path: &Path,
    entry: &Path,
) -> Result<()> {
    let src_dir = scratch.join("src");
    std::fs::create_dir_all(&src_dir)?;

    // Encode a TOML string, including quotes/backslashes in a selected SDK path.
    // Strip any Windows verbatim (`\\?\`) prefix first (the caller hands a
    // canonicalized SDK path) so the generated `Cargo.toml`'s
    // `idtx-behavior-sdk = { path = "…" }` dependency is a plain path — cargo and
    // MSVC `link.exe` resolve the legacy form, not the verbatim one.
    let sdk_path = crate::simplified(sdk_path.to_path_buf());
    let sdk_path_str = toml::Value::String(path_text(&sdk_path)?.to_owned()).to_string();
    let cargo = CARGO_TMPL
        .replace("{{CRATE_NAME}}", crate_name)
        .replace("{{SDK_PATH}}", &sdk_path_str);
    write_if_changed(&scratch.join("Cargo.toml"), cargo.as_bytes())?;

    // Debug formatting a UTF-8 str produces an escaped Rust string literal,
    // including quotes, backslashes, control characters and Unicode paths.
    let entry = crate::simplified(entry.to_path_buf());
    let wrapper = LIB_SHIM.replace("\"{{BEHAVIOR_PATH}}\"", &format!("{:?}", path_text(&entry)?));
    write_if_changed(&src_dir.join("lib.rs"), wrapper.as_bytes())?;

    Ok(())
}

pub(crate) fn path_text(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| crate::BuildError::InvalidInput(format!("path is not UTF-8: {}", path.display())))
}

/// Preserve mtimes for unchanged compiler inputs, so Cargo can report freshness.
pub(crate) fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<()> {
    match std::fs::read(path) {
        Ok(old) if old == bytes => return Ok(()),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    std::fs::write(path, bytes)?;
    Ok(())
}

pub(crate) fn recipe_inputs() -> [&'static str; 2] { [CARGO_TMPL, LIB_SHIM] }

/// Derive a stable pseudo-UUID from the name + a short source hash. Not an
/// RFC-4122 UUID — just a deterministic identifier so a created behavior gets a
/// unique-ish `idtx:behavior:uuid` without pulling a uuid crate.
#[must_use]
pub fn synthesize_uuid(name: &str, source: &str) -> String {
    use core::fmt::Write as _;
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(name.as_bytes());
    h.update(b"\0");
    h.update(source.as_bytes());
    let digest = h.finalize();
    // First 16 bytes, hex.
    let mut s = String::with_capacity(32);
    for b in &digest[..16] {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Render the author-facing behavior module skeleton for a new behavior
/// named `name` (the file stem) attached to `prim_path`, with the given `kind`
/// token (`"runtime"` / `"editor"`).
///
/// The returned string is the `.rs` source the Conductor writes into
/// `<stage-dir>/behaviors/<name>/behavior.rs` for the user to edit. Every lifecycle hook is
/// present with a `todo!()` body so the developer has a complete starting point.
#[must_use]
pub fn render_author_skeleton(name: &str, prim_path: &str, kind: &str) -> String {
    AUTHOR_BEHAVIOR_TMPL
        .replace("{{BEHAVIOR_NAME}}", name)
        .replace("{{BEHAVIOR_TYPE}}", &pascal_case(name))
        .replace("{{PRIM_PATH}}", prim_path)
        .replace("{{KIND}}", kind)
}

/// Convert a behavior file stem into a `UpperCamelCase` type name.
/// Non-alphanumeric characters split words; a leading digit is prefixed with `B`
/// so the result is a valid Rust identifier.
#[must_use]
pub fn pascal_case(stem: &str) -> String {
    let mut out = String::with_capacity(stem.len());
    let mut upper_next = true;
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() {
            if upper_next {
                out.extend(c.to_uppercase());
            } else {
                out.push(c);
            }
            upper_next = false;
        } else {
            upper_next = true;
        }
    }
    if out.is_empty() {
        out.push_str("Behavior");
    }
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, 'B');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_materialises() {
        let directory = tempfile::tempdir().unwrap();
        let tmp = directory.path();
        materialise(
            &tmp,
            "test_beh",
            Path::new("/some/sdk"),
            Path::new("authored/spin/behavior.rs"),
        )
        .expect("materialise");

        let cargo = std::fs::read_to_string(tmp.join("Cargo.toml")).unwrap();
        assert!(cargo.contains("name = \"test_beh\""));
        assert!(cargo.contains("/some/sdk"));
        assert!(cargo.contains("crate-type = [\"cdylib\"]"));

        let lib = std::fs::read_to_string(tmp.join("src/lib.rs")).unwrap();
        assert!(lib.contains("#[path = \"authored/spin/behavior.rs\"]\nmod behavior;"));
        assert!(!tmp.join("src/behavior.rs").exists(), "the wrapper must not create an editable shadow copy");

        // No sidecar manifest is written any more — behavior config lives on the
        // IdtxBehaviorAPI schema attributes, not an `idtx.toml`.
        assert!(!tmp.join("idtx.toml").exists());
    }

    #[test]
    fn uuid_is_deterministic() {
        let a = synthesize_uuid("x", "src");
        let b = synthesize_uuid("x", "src");
        let c = synthesize_uuid("y", "src");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 32);
    }

    #[test]
    fn author_skeleton_substitutes_and_parses() {
        let src = render_author_skeleton("cube_behavior", "/World/Cube", "runtime");
        assert!(src.contains("use idtx_behavior_sdk::prelude::*;"));
        assert!(src.contains("#[behavior(kind = \"runtime\")]"));
        assert!(src.contains("pub struct CubeBehavior"));
        assert!(src.contains("impl CubeBehavior"));
        assert!(src.contains("/World/Cube"));
        assert!(src.contains("mod motion;"));
    }

    #[test]
    fn pascal_case_covers_edge_cases() {
        assert_eq!(pascal_case("cube_behavior"), "CubeBehavior");
        assert_eq!(pascal_case("my-cool_thing"), "MyCoolThing");
        assert_eq!(pascal_case("2fast"), "B2fast");
        assert_eq!(pascal_case("___"), "Behavior");
    }
}
