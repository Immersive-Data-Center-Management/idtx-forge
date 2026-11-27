//! `cargo xtask gen-schema` — regenerate the `OpenUSD` schema plugin resources by
//! running upstream `usdGenSchema`.
//!
//! This drives `usdGenSchema` against a Python-enabled `OpenUSD` install for the
//! IDTX schema crates. Two crates carry a `schema/schema.usda`:
//!
//! * **`idtx-usd-extension`** — a *codeless* IsA/HasAPI schema (no C++ library).
//!   `usdGenSchema` emits only `generatedSchema.usda` + `plugInfo.json`.
//! * **`idtx-usd-exec`** — a *coded* `OpenExec` dataflow plugin (schema classes +
//!   `EXEC_REGISTER_COMPUTATIONS_FOR_SCHEMA` computations compiled into a DLL).
//!   `usdGenSchema` emits the C++ classes too, but it does **not** emit the
//!   `"Exec" > "Schemas"` discovery block the exec framework needs; this tool
//!   re-injects it after generation (see [`inject_exec_schemas`]).
//!
//! By default both crates are regenerated; `--crate <name>` limits to one.
//!
//! The install prefix comes from `--usd-gen-dir` or `$IDTX_USD_GEN_DIR` (a
//! Python-enabled build shipping `bin/usdGenSchema` and the `pxr` python
//! modules). There is **no** `-withPython` suffix concatenation — the variable
//! points directly at the Python-enabled install.
//!
//! # Testability (TMDD)
//!
//! Running the tool needs a Python-enabled `OpenUSD` install (absent on an
//! ordinary CI host), so the *pure* logic — resolving the prefix, picking the
//! binary name, assembling the env overlay, and the Exec-block injection — is
//! factored into free functions unit-tested below. Only the thin orchestration
//! ([`run`]) shells out, failing fast with a clear message when something is
//! missing.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::Args;

use crate::workspace_root;

/// A schema crate that `gen-schema` can regenerate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SchemaCrate {
    /// Crate directory under `crates/`.
    dir: &'static str,
    /// When `Some`, the crate is a *coded* `OpenExec` plugin and these schema
    /// type names must be listed in a re-injected `"Exec" > "Schemas"` block
    /// (usdGenSchema does not emit it). When `None`, the crate is codeless and
    /// the generated `plugInfo.json` is used verbatim.
    exec_schemas: Option<&'static [&'static str]>,
}

/// Every schema crate regenerated when no `--crate` filter is given.
const SCHEMA_CRATES: &[SchemaCrate] = &[
    SchemaCrate {
        dir: "idtx-usd-extension",
        exec_schemas: None,
    },
    SchemaCrate {
        dir: "idtx-usd-exec",
        exec_schemas: Some(&[
            "IDTXCompute_Environment",
            "IDTXCompute_ValueFromAASJson",
            "IDTXCompute_ValueFromJson",
            "IDTXCompute_VarReplacements",
            "IDTXComputeValueSink",
        ]),
    },
];

/// Parsed `gen-schema` options.
#[derive(Debug, Clone, PartialEq, Eq, Default, Args)]
pub struct GenSchemaOptions {
    /// Explicit Python-enabled `OpenUSD` install prefix (overrides
    /// `$IDTX_USD_GEN_DIR`).
    #[arg(long = "usd-gen-dir", value_name = "PATH")]
    pub usd_gen_dir: Option<PathBuf>,
    /// Limit regeneration to this single crate directory (default: all).
    #[arg(long = "crate", value_name = "NAME")]
    pub only_crate: Option<String>,
    /// Resolve the toolchain + assemble the plan, print it, but do not shell out.
    #[arg(long)]
    pub dry_run: bool,
}

/// The schema source file, relative to a schema crate root.
const SCHEMA_SRC_RELPATH: &str = "schema/schema.usda";

/// The output directory for the generated resources, relative to a schema crate
/// root. `usdGenSchema` writes `generatedSchema.usda` + `plugInfo.json` here (the
/// files the runtime reads); for the coded exec crate it also writes the C++
/// classes, which go to `cpp/` via [`CPP_OUT_RELPATH`].
const RESOURCES_RELPATH: &str = "resources";

/// The C++ output directory for a coded schema crate, relative to its root.
const CPP_OUT_RELPATH: &str = "cpp";

/// Resolve the Python-enabled `OpenUSD` install prefix.
///
/// Precedence: explicit `--usd-gen-dir` → `$IDTX_USD_GEN_DIR`.
fn resolve_gen_dir(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(dir) = explicit {
        return Ok(dir);
    }
    if let Some(dir) = env::var_os("IDTX_USD_GEN_DIR") {
        return Ok(PathBuf::from(dir));
    }
    bail!(
        "no Python-enabled OpenUSD install found.\n\
         Set $IDTX_USD_GEN_DIR (or pass --usd-gen-dir <path>) to a Python-enabled\n\
         OpenUSD install prefix (one that ships bin/usdGenSchema and the pxr python\n\
         modules, e.g. thirdparty/openusd-26.08-withPython)."
    )
}

/// Path to the `usdGenSchema` launcher inside an install prefix.
///
/// Windows ships a `.cmd` wrapper; other platforms ship the bare script.
fn genschema_binary(prefix: &Path) -> PathBuf {
    let name = if cfg!(windows) {
        "usdGenSchema.cmd"
    } else {
        "usdGenSchema"
    };
    prefix.join("bin").join(name)
}

/// The directory holding the `pxr` python package inside an install prefix.
///
/// `OpenUSD` installs vary: a `build_usd.py` tree uses `lib/python`, while some
/// CMake installs use `lib/site-packages`. Pick whichever actually contains
/// `pxr/__init__.py`, defaulting to `lib/python` when neither is present (so the
/// error surfaces as a clear "module not found" from the tool).
fn python_module_dir(prefix: &Path) -> PathBuf {
    let site = prefix.join("lib").join("site-packages");
    if site.join("pxr").join("__init__.py").is_file() {
        return site;
    }
    prefix.join("lib").join("python")
}

/// Build the `(USD_ROOT, PYTHONPATH, PATH)` environment overlay the tool needs.
///
/// `PATH` is `<prefix>/bin` + `<prefix>/lib` prepended to the inherited `PATH`.
fn genschema_env(
    prefix: &Path,
    inherited_path: Option<OsString>,
) -> Result<(OsString, OsString, OsString)> {
    let usd_root = prefix.as_os_str().to_os_string();
    let pythonpath = python_module_dir(prefix).into_os_string();

    let mut path_entries = vec![prefix.join("bin"), prefix.join("lib")];
    if let Some(existing) = inherited_path {
        for p in env::split_paths(&existing) {
            path_entries.push(p);
        }
    }
    let path = env::join_paths(path_entries).context("assembling PATH for usdGenSchema")?;

    Ok((usd_root, pythonpath, path))
}

/// Re-inject the hand-maintained `"Exec" > "Schemas"` discovery block that
/// `usdGenSchema` does not emit.
///
/// Operates on the `plugInfo.json` *text* (not a JSON parse) so the `#` comment
/// header and the `@PLUG_INFO_*@` path placeholders survive untouched — the same
/// text-surgery philosophy as the extension crate's build.rs. The block is
/// inserted immediately after the `"Info": {` line, matching its indentation.
/// Idempotent: if an `"Exec"` key is already present the input is returned
/// unchanged.
fn inject_exec_schemas(plug_info: &str, schemas: &[&str]) -> Result<String> {
    if plug_info.contains("\"Exec\"") {
        return Ok(plug_info.to_string());
    }
    let marker = "\"Info\": {";
    let Some(info_line_start) = plug_info.find(marker) else {
        bail!("plugInfo.json has no `\"Info\": {{` block to inject the Exec schemas into");
    };
    // Indentation of the `"Info": {` line → children indent one step (4 spaces)
    // deeper, matching usdGenSchema's 4-space style.
    let line_indent_len = plug_info[..info_line_start]
        .chars()
        .rev()
        .take_while(|c| *c == ' ')
        .count();
    let child_indent = " ".repeat(line_indent_len + 4);
    let inner_indent = " ".repeat(line_indent_len + 8);
    let entry_indent = " ".repeat(line_indent_len + 12);

    let mut block = String::new();
    block.push('\n');
    block.push_str(&child_indent);
    block.push_str("\"Exec\": {\n");
    block.push_str(&inner_indent);
    block.push_str("\"Schemas\": {\n");
    for (i, s) in schemas.iter().enumerate() {
        block.push_str(&entry_indent);
        block.push_str(&format!("\"{s}\": {{}}"));
        block.push_str(if i + 1 < schemas.len() { ",\n" } else { "\n" });
    }
    block.push_str(&inner_indent);
    block.push_str("}\n"); // close "Schemas"
    block.push_str(&child_indent);
    block.push_str("},"); // close "Exec" (trailing comma: the original Info members follow)

    // Insert right after the `"Info": {` marker.
    let insert_at = info_line_start + marker.len();
    let mut out = String::with_capacity(plug_info.len() + block.len());
    out.push_str(&plug_info[..insert_at]);
    out.push_str(&block);
    out.push_str(&plug_info[insert_at..]);
    Ok(out)
}

/// Entry point for `cargo xtask gen-schema`.
pub fn gen_schema_cmd(opts: &GenSchemaOptions) -> Result<()> {
    run(opts)
}

fn run(opts: &GenSchemaOptions) -> Result<()> {
    let prefix = resolve_gen_dir(opts.usd_gen_dir.clone())?;
    let binary = genschema_binary(&prefix);
    let workspace_root = workspace_root()?;

    let targets: Vec<&SchemaCrate> = match &opts.only_crate {
        Some(name) => {
            let t = SCHEMA_CRATES
                .iter()
                .find(|c| c.dir == name)
                .with_context(|| {
                    format!(
                        "unknown schema crate `{name}` (known: {})",
                        SCHEMA_CRATES
                            .iter()
                            .map(|c| c.dir)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })?;
            vec![t]
        }
        None => SCHEMA_CRATES.iter().collect(),
    };

    let (usd_root, pythonpath, path) = genschema_env(&prefix, env::var_os("PATH"))?;

    println!("Generate OpenUSD schema code from schema.usda...");
    println!("   USD_ROOT   = {}", usd_root.to_string_lossy());
    println!("   PYTHONPATH = {}", pythonpath.to_string_lossy());
    println!("   tool       = {}", binary.display());
    println!(
        "   crates     = {}",
        targets.iter().map(|c| c.dir).collect::<Vec<_>>().join(", ")
    );

    if opts.dry_run {
        println!("(dry run — not executing)");
        return Ok(());
    }

    if !binary.is_file() {
        bail!(
            "usdGenSchema not found at {}.\n\
             Is the resolved prefix a Python-enabled OpenUSD install?",
            binary.display()
        );
    }

    for target in targets {
        gen_one(target, &workspace_root, &binary, &usd_root, &pythonpath, &path)?;
    }
    Ok(())
}

/// Regenerate one schema crate.
fn gen_one(
    target: &SchemaCrate,
    workspace_root: &Path,
    binary: &Path,
    usd_root: &OsString,
    pythonpath: &OsString,
    path: &OsString,
) -> Result<()> {
    let crate_root = workspace_root.join("crates").join(target.dir);
    let schema_src = crate_root.join(SCHEMA_SRC_RELPATH);
    if !schema_src.is_file() {
        bail!("schema source not found at {}", schema_src.display());
    }

    // Coded exec crates emit C++ classes into cpp/; codeless crates only touch
    // resources/. usdGenSchema always writes generatedSchema.usda + plugInfo.json
    // into its single output directory, so coded crates generate into cpp/ and we
    // then relocate the two resource files into resources/.
    let out_dir = if target.exec_schemas.is_some() {
        crate_root.join(CPP_OUT_RELPATH)
    } else {
        crate_root.join(RESOURCES_RELPATH)
    };

    println!("\n== {} ==", target.dir);
    println!(
        "   command    = usdGenSchema {SCHEMA_SRC_RELPATH} {}",
        out_dir
            .strip_prefix(&crate_root)
            .unwrap_or(&out_dir)
            .display()
    );

    let status = Command::new(binary)
        .current_dir(&crate_root)
        .arg(SCHEMA_SRC_RELPATH)
        .arg(&out_dir)
        .env("USD_ROOT", usd_root)
        .env("PYTHONPATH", pythonpath)
        .env("PATH", path)
        .status()
        .with_context(|| format!("failed to spawn {}", binary.display()))?;
    if !status.success() {
        bail!("usdGenSchema failed with {status} for {}", target.dir);
    }

    let resources = crate_root.join(RESOURCES_RELPATH);
    if let Some(schemas) = target.exec_schemas {
        // Coded crate: discard the python wrappers usdGenSchema emitted (we build
        // no python bindings), relocate the two resource files into resources/,
        // and re-inject the Exec discovery block into plugInfo.json.
        remove_wrap_cpp(&out_dir)?;
        fs::create_dir_all(&resources)?;
        relocate(&out_dir, &resources, "generatedSchema.usda")?;

        let generated_plug = out_dir.join("plugInfo.json");
        let plug_text = fs::read_to_string(&generated_plug)
            .with_context(|| format!("read {}", generated_plug.display()))?;
        let injected = inject_exec_schemas(&plug_text, schemas)?;
        fs::write(resources.join("plugInfo.json"), injected)
            .with_context(|| format!("write {}", resources.join("plugInfo.json").display()))?;
        fs::remove_file(&generated_plug).ok();

        println!(
            "   \u{2705} C++ classes in {}; resources (generatedSchema.usda + \
             plugInfo.json w/ Exec block) in {}",
            out_dir.display(),
            resources.display()
        );
        println!(
            "      Review the diff: hand-written computation_*.cpp / \
             EnvironmentProvider* are NOT touched."
        );
    } else {
        println!("   \u{2705} Regenerated schema resources in {}", resources.display());
        println!(
            "      Review the diff: usdGenSchema may re-add a per-type \"alias\" block to\n\
             \x20     plugInfo.json — strip it for the codeless schema (see the crate README)."
        );
    }
    Ok(())
}

/// Delete the `wrap*.cpp` python-binding sources usdGenSchema emits; the IDTX
/// plugins build no python bindings.
fn remove_wrap_cpp(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read dir {}", dir.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("wrap") && name.ends_with(".cpp") {
            fs::remove_file(entry.path()).ok();
        }
    }
    Ok(())
}

/// Move a generated file from `from_dir` into `to_dir`, overwriting.
fn relocate(from_dir: &Path, to_dir: &Path, file: &str) -> Result<()> {
    let src = from_dir.join(file);
    let dst = to_dir.join(file);
    fs::copy(&src, &dst).with_context(|| format!("copy {} -> {}", src.display(), dst.display()))?;
    fs::remove_file(&src).ok();
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Minimal clap wrapper so the `#[derive(Args)]` struct can be parsed from a
    /// bare argument vector in tests.
    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        opts: GenSchemaOptions,
    }

    fn parse(args: &[&str]) -> Result<GenSchemaOptions, clap::Error> {
        let argv = std::iter::once("gen-schema").chain(args.iter().copied());
        TestCli::try_parse_from(argv).map(|c| c.opts)
    }

    #[test]
    fn parse_defaults() {
        let opts = parse(&[]).unwrap();
        assert!(opts.usd_gen_dir.is_none());
        assert!(opts.only_crate.is_none());
        assert!(!opts.dry_run);
    }

    #[test]
    fn parse_flags() {
        let opts = parse(&["--dry-run", "--usd-gen-dir", "/opt/usd-py", "--crate", "idtx-usd-exec"])
            .unwrap();
        assert!(opts.dry_run);
        assert_eq!(opts.usd_gen_dir, Some(PathBuf::from("/opt/usd-py")));
        assert_eq!(opts.only_crate.as_deref(), Some("idtx-usd-exec"));
    }

    #[test]
    fn parse_missing_dir_value_errors() {
        assert!(parse(&["--usd-gen-dir"]).is_err());
    }

    #[test]
    fn parse_missing_crate_value_errors() {
        assert!(parse(&["--crate"]).is_err());
    }

    #[test]
    fn parse_unknown_arg_errors() {
        assert!(parse(&["--bogus"]).is_err());
    }

    #[test]
    fn both_crates_are_known_targets() {
        assert_eq!(SCHEMA_CRATES.len(), 2);
        assert!(SCHEMA_CRATES.iter().any(|c| c.dir == "idtx-usd-extension" && c.exec_schemas.is_none()));
        assert!(SCHEMA_CRATES.iter().any(|c| c.dir == "idtx-usd-exec" && c.exec_schemas.is_some()));
    }

    #[test]
    fn resolve_prefers_explicit() {
        let dir = resolve_gen_dir(Some(PathBuf::from("/explicit"))).unwrap();
        assert_eq!(dir, PathBuf::from("/explicit"));
    }

    #[test]
    fn binary_name_is_platform_specific() {
        let bin = genschema_binary(Path::new("/opt/usd"));
        if cfg!(windows) {
            assert!(bin.ends_with("bin/usdGenSchema.cmd"));
        } else {
            assert!(bin.ends_with("bin/usdGenSchema"));
        }
    }

    #[test]
    fn env_sets_usd_root() {
        let prefix = Path::new("/opt/usd-py");
        let (usd_root, _pythonpath, _path) = genschema_env(prefix, None).unwrap();
        assert_eq!(usd_root, prefix.as_os_str());
    }

    #[test]
    fn env_path_prepends_bin_and_lib() {
        let prefix = Path::new("/opt/usd-py");
        let inherited = env::join_paths([PathBuf::from("/usr/bin")]).unwrap();
        let (_usd_root, _pythonpath, path) = genschema_env(prefix, Some(inherited)).unwrap();
        let entries: Vec<PathBuf> = env::split_paths(&path).collect();
        assert_eq!(entries[0], prefix.join("bin"));
        assert_eq!(entries[1], prefix.join("lib"));
        assert!(entries.contains(&PathBuf::from("/usr/bin")));
    }

    #[test]
    fn inject_exec_adds_block_after_info() {
        let input = "{\n    \"Plugins\": [\n        {\n            \"Info\": {\n                \"Types\": {}\n            }\n        }\n    ]\n}\n";
        let out = inject_exec_schemas(input, &["IDTXCompute_ValueFromJson"]).unwrap();
        assert!(out.contains("\"Exec\": {"));
        assert!(out.contains("\"Schemas\": {"));
        assert!(out.contains("\"IDTXCompute_ValueFromJson\": {}"));
        // Exec appears before the Types block (inserted right after `"Info": {`).
        assert!(out.find("\"Exec\"").unwrap() < out.find("\"Types\"").unwrap());
        // The result must still be well-formed JSON — the Exec AND Schemas objects
        // both close, and Types stays a sibling of Exec (not nested inside it).
        let v: serde_json::Value = serde_json::from_str(&out).expect("injected plugInfo is valid JSON");
        let info = &v["Plugins"][0]["Info"];
        assert!(info["Types"].is_object(), "Types must remain a sibling of Exec");
    }

    #[test]
    fn inject_exec_is_idempotent() {
        let input = "{ \"Info\": { \"Exec\": { \"Schemas\": {} } } }";
        let out = inject_exec_schemas(input, &["X"]).unwrap();
        assert_eq!(out, input);
    }

    #[test]
    fn inject_exec_preserves_placeholders_and_comments() {
        let input = "# comment\n{\n    \"Info\": {\n        \"LibraryPath\": \"@PLUG_INFO_LIBRARY_PATH@\"\n    }\n}\n";
        let out = inject_exec_schemas(input, &["X"]).unwrap();
        assert!(out.starts_with("# comment"));
        assert!(out.contains("@PLUG_INFO_LIBRARY_PATH@"));
        assert!(out.contains("\"X\": {}"));
    }

    #[test]
    fn inject_exec_errors_without_info() {
        assert!(inject_exec_schemas("{}", &["X"]).is_err());
    }
}
