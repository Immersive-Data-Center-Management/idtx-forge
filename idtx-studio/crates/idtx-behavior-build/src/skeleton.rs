//! Materialise the embedded cargo skeleton into a scratch dir.
//!
//! The skeleton (`crates/idtx-behavior-build/skeleton/`) is embedded via
//! `include_str!` so the compiled `idtx-behavior-build` needs no files on
//! disk. Materialisation writes:
//!
//! ```text
//! <scratch>/
//!   Cargo.toml           (from Cargo.toml.tmpl, placeholders substituted)
//!   src/lib.rs           (the include! shim)
//!   src/behavior.rs      (the author's single-file source, verbatim)
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
/// SDK path into `Cargo.toml`, and copy the author's `source` into
/// `src/behavior.rs`.
pub fn materialise(
    scratch: &Path,
    crate_name: &str,
    sdk_path: &Path,
    source: &str,
) -> Result<()> {
    let src_dir = scratch.join("src");
    std::fs::create_dir_all(&src_dir)?;

    // Encode a TOML string, including quotes/backslashes in a selected SDK path.
    // Strip any Windows verbatim (`\\?\`) prefix first (the caller hands a
    // canonicalized SDK path) so the generated `Cargo.toml`'s
    // `idtx-behavior-sdk = { path = "…" }` dependency is a plain path — cargo and
    // MSVC `link.exe` resolve the legacy form, not the verbatim one.
    let sdk_path = crate::simplified(sdk_path.to_path_buf());
    let sdk_path_str = toml::Value::String(sdk_path.to_string_lossy().into_owned()).to_string();
    let cargo = CARGO_TMPL
        .replace("{{CRATE_NAME}}", crate_name)
        .replace("{{SDK_PATH}}", &sdk_path_str);
    write_if_changed(&scratch.join("Cargo.toml"), cargo.as_bytes())?;

    // src/lib.rs shim + the author's source.
    write_if_changed(&src_dir.join("lib.rs"), LIB_SHIM.as_bytes())?;
    write_if_changed(&src_dir.join("behavior.rs"), source.as_bytes())?;

    Ok(())
}

/// Stage direct sibling Rust files so literal include!/path modules retain
/// their authoring layout. Only generated scratch/src files are removed when
/// companions disappear. Symlinks are rejected instead of escaping the source.
pub(crate) fn stage_companions(scratch: &Path, source_file: &Path) -> Result<()> {
    use std::collections::BTreeMap;
    use crate::BuildError;
    let source_dir = source_file.parent().ok_or_else(|| BuildError::InvalidInput("source has no parent".into()))?;
    let source_name = source_file.file_name().ok_or_else(|| BuildError::InvalidInput("source has no file name".into()))?;
    let src_dir = scratch.join("src");
    let mut companions = BTreeMap::new();
    for entry in std::fs::read_dir(source_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == source_name || name == "lib.rs" || name == "behavior.rs"
            || entry.path().extension().is_none_or(|extension| extension != "rs") { continue; }
        if !entry.file_type()?.is_file() || entry.file_type()?.is_symlink() {
            return Err(BuildError::InvalidInput(format!("Rust companion must be a regular file: {}", entry.path().display())));
        }
        companions.insert(name, std::fs::read(entry.path())?);
    }
    for entry in std::fs::read_dir(&src_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "lib.rs" || name == "behavior.rs"
            || entry.path().extension().is_none_or(|extension| extension != "rs") { continue; }
        if !entry.file_type()?.is_file() || entry.file_type()?.is_symlink() {
            return Err(BuildError::InvalidInput("generated companion is not a regular file".into()));
        }
        if !companions.contains_key(&name) { std::fs::remove_file(entry.path())?; }
    }
    for (name, bytes) in companions { write_if_changed(&src_dir.join(name), &bytes)?; }
    Ok(())
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

/// Render the author-facing single-file behavior skeleton for a new behavior
/// named `name` (the file stem) attached to `prim_path`, with the given `kind`
/// token (`"runtime"` / `"editor"`).
///
/// The returned string is the `.rs` source the Conductor writes into
/// `<project>/behaviors/<name>.rs` for the user to edit. Every lifecycle hook is
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
        let tmp = std::env::temp_dir().join(format!("idtx-skel-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        materialise(
            &tmp,
            "test_beh",
            Path::new("/some/sdk"),
            "// author source\n",
        )
        .expect("materialise");

        let cargo = std::fs::read_to_string(tmp.join("Cargo.toml")).unwrap();
        assert!(cargo.contains("name = \"test_beh\""));
        assert!(cargo.contains("/some/sdk"));
        assert!(cargo.contains("crate-type = [\"cdylib\"]"));

        let lib = std::fs::read_to_string(tmp.join("src/lib.rs")).unwrap();
        assert!(lib.contains("include!(\"behavior.rs\")"));

        let beh = std::fs::read_to_string(tmp.join("src/behavior.rs")).unwrap();
        assert_eq!(beh, "// author source\n");

        // No sidecar manifest is written any more — behavior config lives on the
        // IdtxBehaviorAPI schema attributes, not an `idtx.toml`.
        assert!(!tmp.join("idtx.toml").exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn companions_update_and_stale_includes_are_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let author = tmp.path().join("author");
        let scratch = tmp.path().join("scratch");
        std::fs::create_dir(&author).unwrap();
        let source = author.join("camera.rs");
        std::fs::write(&source, "include!(\"context.rs\");").unwrap();
        std::fs::write(author.join("context.rs"), "const VALUE: u32 = 1;").unwrap();
        materialise(&scratch, "test", Path::new("/sdk"), "include!(\"context.rs\");").unwrap();
        stage_companions(&scratch, &source).unwrap();
        let companion = scratch.join("src/context.rs");
        assert_eq!(std::fs::read_to_string(&companion).unwrap(), "const VALUE: u32 = 1;");
        std::fs::write(author.join("context.rs"), "const VALUE: u32 = 2;").unwrap();
        stage_companions(&scratch, &source).unwrap();
        assert_eq!(std::fs::read_to_string(&companion).unwrap(), "const VALUE: u32 = 2;");
        std::fs::remove_file(author.join("context.rs")).unwrap();
        stage_companions(&scratch, &source).unwrap();
        assert!(!companion.exists(), "deleted includes must not silently use stale scratch content");
        assert!(scratch.join("src/lib.rs").exists());
        assert!(scratch.join("src/behavior.rs").exists());
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
        // The generated file is `include!`d beneath a `#![no_std]` shim: no
        // line may be an inner doc comment (`//!`). (The template's prose may
        // still mention the token, so check line starts, not a substring.)
        assert!(!src.lines().any(|l| l.trim_start().starts_with("//!")));
    }

    #[test]
    fn pascal_case_covers_edge_cases() {
        assert_eq!(pascal_case("cube_behavior"), "CubeBehavior");
        assert_eq!(pascal_case("my-cool_thing"), "MyCoolThing");
        assert_eq!(pascal_case("2fast"), "B2fast");
        assert_eq!(pascal_case("___"), "Behavior");
    }
}
