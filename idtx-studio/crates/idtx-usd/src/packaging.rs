#[allow(clippy::wildcard_imports)]
use crate::*;
use idtx_usd_sys::bridge::ffi;
use std::path::{Path, PathBuf};

/// Selects whether a package is an editable asset or a runtime artifact.
///
/// Imports and authoring saves must preserve source bindings and source bytes.
/// Runtime cooks strip design-time Rust sources after compiling behaviors.
/// Loose runtime staging and `copy_runtime_asset` use the same policy; cooks
/// additionally enforce it over their complete staged inventory before hashing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackagingPurpose {
    /// Retain source bindings and bytes for imports and authoring saves.
    PreserveAuthoring,
    /// Strip design-time Rust sources from cooked USDZ artifacts.
    Runtime,
}

impl PackagingPurpose {
    fn options(self) -> ffi::PackagingOptionsRaw {
        ffi::PackagingOptionsRaw {
            strip_behavior_sources: self == Self::Runtime,
        }
    }
}

/// Outcome of a packaging or dependency computation: the destination, the
/// resolved dependency closure, references that could not be resolved, remote
/// references deliberately kept external, and `OpenUSD` warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageResult {
    pub destination: PathBuf,
    pub dependencies: Vec<PathBuf>,
    pub unresolved: Vec<String>,
    /// Remote (`http(s)://`) references deliberately kept EXTERNAL — left
    /// authored in the produced package, never fetched or embedded. Only
    /// populated by [`package_stage_localize_keeping_remote`]; empty otherwise.
    pub external: Vec<String>,
    /// Diagnostics (errors/warnings/status) `OpenUSD` emitted while building the
    /// package — e.g. an asset that failed to localize. Empty on a clean
    /// package and for [`compute_dependencies`] (which performs no packaging).
    pub warnings: Vec<String>,
}

/// Emits a `tracing` line describing the on-disk dependency closure of `path`,
/// highlighting unresolved references. Used by the `persist-diagnostics` build
/// to explain why a save was gated (or to confirm the closure that passed).
///
/// Best-effort: computing dependencies opens the file fresh from disk, so it
/// reflects the last-saved content rather than unsaved in-memory edits. That is
/// exactly what we want for reasoning about references/textures, which are not
/// affected by transform edits. Failures here never affect the save result.
#[cfg(feature = "persist-diagnostics")]
pub(crate) fn log_save_dependencies(context: &str, path: &Path) {
    match compute_dependencies(path) {
        Ok(result) => {
            if result.unresolved.is_empty() {
                tracing::info!(
                    "{context}: {} has {} resolved dependencies, none unresolved",
                    path.display(),
                    result.dependencies.len()
                );
            } else {
                tracing::warn!(
                    "{context}: {} has {} unresolved dependencies (save tolerates \
                     pre-existing unresolved refs): {:?}",
                    path.display(),
                    result.unresolved.len(),
                    result.unresolved
                );
            }
        }
        Err(err) => {
            tracing::debug!(
                "{context}: could not compute dependencies for {}: {err}",
                path.display()
            );
        }
    }
}

/// Computes the official recursive `OpenUSD` dependency set without creating
/// a package, allowing content-addressed cache lookup before localization.
pub fn compute_dependencies(source: impl AsRef<Path>) -> Result<PackageResult, StageError> {
    register_plugins_once()?;
    let source = source.as_ref();
    let source_text = source
        .to_str()
        .ok_or_else(|| StageError::NonUtf8Path(source.to_path_buf()))?;
    Ok(package_result_from_raw(ffi::compute_dependencies(
        source_text,
    )?))
}

/// Copy a runtime asset without design-time Rust dependencies/source bindings.
/// Layer clones include dormant variants; USDZ copying covers every member and
/// nested USDZ, preserving order and unrelated bytes. Sources and live authoring
/// layers are read-only. Returns whether runtime filtering changed the bytes.
pub fn copy_runtime_asset(source: &Path, destination: &Path) -> Result<bool, StageError> {
    register_plugins_once()?;
    Ok(ffi::copy_runtime_asset(&strip_verbatim_fwd_slash(source)?, &strip_verbatim_fwd_slash(destination)?)?)
}

/// Design-time source identity, including URL queries and package-member paths.
pub fn is_behavior_source_asset(path: &str) -> bool {
    let path = if is_remote_uri(path) { path.split(['?', '#']).next().unwrap_or(path) } else { path };
    let path = path.rsplit('[').next().unwrap_or(path).trim_end_matches(']');
    Path::new(path).extension().is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
}

/// Uses official `OpenUSD` dependency localization to create a verified USDZ.
/// Use [`PackagingPurpose::PreserveAuthoring`] for imports and editable assets;
/// only runtime cooks should opt into [`PackagingPurpose::Runtime`].
pub fn package_stage(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
    purpose: PackagingPurpose,
) -> Result<PackageResult, StageError> {
    register_plugins_once()?;
    let source = source.as_ref();
    let destination = destination.as_ref();
    let source_text = source
        .to_str()
        .ok_or_else(|| StageError::NonUtf8Path(source.to_path_buf()))?;
    let destination_text = destination
        .to_str()
        .ok_or_else(|| StageError::NonUtf8Path(destination.to_path_buf()))?;
    Ok(package_result_from_raw(ffi::package_stage(
        source_text,
        destination_text,
        &purpose.options(),
    )?))
}

/// "Import anyway" packaging: create a verified USDZ from `source` while
/// dropping references that cannot be resolved (missing textures, references,
/// payloads, or shader-source URIs), instead of failing like [`package_stage`]
/// does on unresolved dependencies.
///
/// The on-disk `source` files are never modified — the unresolved asset paths
/// are cleared from in-memory copies of the dependency-closure layers, and
/// `OpenUSD` packages those copies (never re-reading or saving the originals).
/// The returned [`PackageResult::unresolved`] lists which references were
/// dropped.
///
/// Resolvable behavior sources are retained with
/// [`PackagingPurpose::PreserveAuthoring`], even when other assets are dropped.
pub fn package_stage_dropping_unresolved(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
    purpose: PackagingPurpose,
) -> Result<PackageResult, StageError> {
    register_plugins_once()?;
    let source = source.as_ref();
    let destination = destination.as_ref();
    let source_text = source
        .to_str()
        .ok_or_else(|| StageError::NonUtf8Path(source.to_path_buf()))?;
    let destination_text = destination
        .to_str()
        .ok_or_else(|| StageError::NonUtf8Path(destination.to_path_buf()))?;
    Ok(package_result_from_raw(
        ffi::package_stage_dropping_unresolved(
            source_text,
            destination_text,
            &purpose.options(),
        )?,
    ))
}

/// `true` when `s` is a remote (`http(s)://`) asset reference. Matches both the
/// authored `scheme://host/…` form (when the HTTP URI resolver is registered)
/// and the `scheme:/host/…` single-slash form the default resolver produces when
/// no URI resolver claims the scheme and it collapses `//` during anchoring
/// (`http://…` begins with `http:/`, so one prefix test covers both).
pub(crate) fn is_remote_uri(s: &str) -> bool {
    let s = s.trim_start();
    s.starts_with("http:/") || s.starts_with("https:/")
}

/// `true` when `source`'s dependency closure contains any remote (`http(s)://`)
/// reference. Used by the cook to decide between the verbatim-zip pack (which
/// preserves remote URLs) and the localize flatten.
///
/// Remote refs surface as **unresolved** only when the HTTP resolver either is
/// not installed or is in packaging mode (so it reports them unresolvable rather
/// than fetching them). The cook arms
/// the HTTP URI resolver's packaging mode before calling this, so this
/// never triggers a network fetch.
pub fn stage_has_remote_refs(source: impl AsRef<Path>) -> Result<bool, StageError> {
    Ok(compute_dependencies(source)?
        .unresolved
        .iter()
        .any(|u| is_remote_uri(u)))
}

/// Packaging that LOCALIZES local references to package-internal paths (so a
/// cooked `.usdz` resolves its own assets on any machine) while PRESERVING remote
/// (`http(s)://`) references verbatim (the original URL survives for the runtime
/// HTTP resolver to fetch at play time).
///
/// USD's one-shot usdz packager ([`package_stage`]) cannot do both: its directory
/// remapper rewrites every absolute/URI reference — including a remote URL — into
/// an internal bucket, destroying the URL. This instead localizes into a temp
/// directory via `UsdUtilsLocalizeAsset` with a processing function that swaps
/// remote URLs for relative, unresolvable sentinels (kept untouched by the
/// localizer while every local ref — search-path, absolute, relative — is
/// package-internalized), restores the sentinels to their URLs in the localized
/// layers, then zips the tree into the `.usdz`. The preserved remote URLs are
/// returned in [`PackageResult::external`].
/// Source retention follows `purpose`, just as in [`package_stage`].
///
/// The caller should arm the HTTP resolver's packaging mode around this call so
/// the remote URLs are not fetched while computing the closure.
pub fn package_stage_localize_keeping_remote(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
    purpose: PackagingPurpose,
) -> Result<PackageResult, StageError> {
    register_plugins_once()?;
    let source = source.as_ref();
    let destination = destination.as_ref();
    // Strip the Windows `\\?\` verbatim prefix before handing these to the
    // hand-rolled localize-and-zip packager. That shim composes child paths as
    // `dir + "/" + rel` and opens them via `SdfZipFileWriter::AddFile`; under the
    // Win32 `\\?\` namespace `/` is NOT a separator, so a verbatim-prefixed
    // destination (produced by the publish step's `fs::canonicalize`) makes every
    // file fail to open ("Failed to map … : No error") and the whole assembly
    // throws. A plain (non-verbatim) path lets Windows normalize the separators.
    // The one-shot `package_stage` does not need this — USD handles the path
    // internally — so only this path strips.
    let source_text = strip_verbatim_fwd_slash(source)?;
    let destination_text = strip_verbatim_fwd_slash(destination)?;
    Ok(package_result_from_raw(
        ffi::package_stage_localize_keeping_remote(
            &source_text,
            &destination_text,
            &purpose.options(),
        )?,
    ))
}

pub(crate) fn package_result_from_raw(raw: ffi::PackageResultRaw) -> PackageResult {
    PackageResult {
        destination: PathBuf::from(raw.destination),
        dependencies: raw.dependencies.into_iter().map(PathBuf::from).collect(),
        unresolved: raw.unresolved,
        external: raw.external,
        warnings: raw.warnings,
    }
}

/// The result of [`stage_loose_closure`] / [`stage_loose_closure_under_root`].
#[derive(Debug, Clone)]
pub struct LooseStageResult {
    /// The staged entry stage, **relative to the `dest_dir`** passed in (i.e. the
    /// path to open the staged scene, joined onto `dest_dir`). References inside
    /// resolve against the staged tree exactly as they did in the source.
    pub staged_entry: PathBuf,
    /// The source dependency closure that was copied (absolute source paths), for
    /// the caller's reproducibility hashing / reporting.
    pub dependencies: Vec<PathBuf>,
    /// Dependencies that fell OUTSIDE the staging root and were therefore **not**
    /// copied (absolute source paths). Empty for [`stage_loose_closure`] (which
    /// errors instead). For [`stage_loose_closure_under_root`] these are refs the
    /// caller is expected to satisfy another way — e.g. a built-in asset tree
    /// staged separately and resolved via the runtime's asset search path. A ref
    /// here that the caller does NOT otherwise stage will dangle at runtime.
    /// Remote (`http(s)://`) references are also reported here — kept external
    /// verbatim (never copied) for the runtime HTTP resolver to fetch.
    pub external: Vec<PathBuf>,
}

/// Stage the dependency closure of `source` as a LOOSE directory tree under
/// `dest_dir`, preserving each dependency's path relative to the closure's
/// **common root** directory. Every dependency must live under that common root
/// or this errors ([`StageError::LooseStage`]). See
/// [`stage_loose_closure_under_root`] for the variant that tolerates
/// out-of-root dependencies.
///
/// Unlike [`package_stage`] — which flattens and localizes the closure into a
/// single `.usdz` through `OpenUSD`'s directory remapper — this copies every layer
/// and asset **verbatim**, so the relative references between them are preserved
/// with no rewriting at all. Two consequences:
///
/// * **Instancing / composition are byte-identical** to the original: the
///   relative-spelling remapper hazard (which can dangle references and fragment
///   point-instancer prototypes, see `package_stage`'s canonicalization) cannot
///   arise because nothing is rewritten.
/// * **No in-package read penalty**: the runtime reads individual files (fast,
///   OS-cached, parallel) instead of resolving every material/texture out of one
///   large `.usdz` archive.
///
/// Unresolved *local* references are refused, mirroring [`package_stage`].
/// Remote (`http(s)://`) references are the one exception: they are kept
/// external verbatim (never copied, never canonicalized) and returned in
/// [`LooseStageResult::external`] for the runtime HTTP resolver to fetch.
pub fn stage_loose_closure(
    source: impl AsRef<Path>,
    dest_dir: impl AsRef<Path>,
) -> Result<LooseStageResult, StageError> {
    stage_loose(source.as_ref(), dest_dir.as_ref(), None, PackagingPurpose::PreserveAuthoring)
}

/// Like [`stage_loose_closure`], but stages relative to an explicit `root`
/// directory (e.g. a project root) and **tolerates** dependencies that fall
/// outside it: those are not copied and are returned in
/// [`LooseStageResult::external`] for the caller to satisfy another way (e.g. a
/// built-in asset tree staged separately and resolved via the asset search
/// path). `source` itself must live under `root`.
pub fn stage_loose_closure_under_root(
    source: impl AsRef<Path>,
    dest_dir: impl AsRef<Path>,
    root: impl AsRef<Path>,
) -> Result<LooseStageResult, StageError> {
    stage_loose(source.as_ref(), dest_dir.as_ref(), Some(root.as_ref()), PackagingPurpose::PreserveAuthoring)
}

/// Explicit cook-only loose staging. Runtime sources are omitted and source
/// bindings in copied layers/packages are stripped without editing the inputs.
pub fn stage_loose_closure_under_root_with_purpose(
    source: impl AsRef<Path>, dest_dir: impl AsRef<Path>, root: impl AsRef<Path>, purpose: PackagingPurpose,
) -> Result<LooseStageResult, StageError> {
    stage_loose(source.as_ref(), dest_dir.as_ref(), Some(root.as_ref()), purpose)
}

pub(crate) fn stage_loose(
    source: &Path,
    dest_dir: &Path,
    root: Option<&Path>,
    purpose: PackagingPurpose,
) -> Result<LooseStageResult, StageError> {
    let deps = compute_dependencies(source)?;

    // Remote (`http(s)://`) references are kept external verbatim in a loose
    // cook: they are never copied into the staged tree and never canonicalized
    // as filesystem paths (`std::fs::canonicalize` on a URL fails with "No such
    // file or directory"). They are surfaced in [`LooseStageResult::external`]
    // for the runtime HTTP resolver to fetch at play time — mirroring the usdz
    // packager's `package_stage_localize_keeping_remote`.
    //
    // A remote ref surfaces in `compute_dependencies` one of two ways depending
    // on resolver state: as an *unresolved* entry (resolver in packaging mode or
    // not installed → reports it unresolvable) or as a resolved URL-string
    // *dependency* (resolver active → returns the URL as its own resolved path).
    // Route it to `external` in both cases rather than erroring / canonicalizing.
    let mut external: Vec<PathBuf> = Vec::new();

    let mut unresolved_local: Vec<String> = Vec::new();
    for u in deps.unresolved {
        if purpose == PackagingPurpose::Runtime && is_behavior_source_asset(&u) { continue; }
        if is_remote_uri(&u) {
            external.push(PathBuf::from(u));
        } else {
            unresolved_local.push(u);
        }
    }
    if !unresolved_local.is_empty() {
        return Err(StageError::UnresolvedAssets(unresolved_local));
    }

    // Canonicalize every *local* closure path to one real form (strip Windows
    // `\\?\` verbatim prefixes, forward-slash) so comparisons/dedup/common-root
    // are consistent with USD's own real-path spelling. `compute_dependencies`
    // returns real absolute paths (layers + assets); include `source` too.
    // Remote URL deps are diverted to `external` (never canonicalized/copied).
    let norm = canon_fwd_slash;

    let mut paths: Vec<String> = Vec::with_capacity(deps.dependencies.len() + 1);
    let source_norm = norm(source)?;
    paths.push(source_norm.clone());
    let mut local_deps: Vec<PathBuf> = Vec::with_capacity(deps.dependencies.len());
    for dep in deps.dependencies {
        if purpose == PackagingPurpose::Runtime && dep.to_str().is_some_and(is_behavior_source_asset) { continue; }
        if dep.to_str().is_some_and(is_remote_uri) {
            external.push(dep);
            continue;
        }
        paths.push(norm(dep.as_path())?);
        local_deps.push(dep);
    }
    paths.sort();
    paths.dedup();

    // The base directory every staged path is made relative to.
    let base = match root {
        Some(r) => norm(r)?.trim_end_matches('/').to_owned(),
        None => longest_common_dir(&paths).ok_or_else(|| {
            StageError::LooseStage("closure has no common root directory".to_owned())
        })?,
    };
    let base_prefix = format!("{base}/");

    for path in &paths {
        let Some(rel) = path.strip_prefix(&base_prefix) else {
            // Outside the staging root. In explicit-root mode this is tolerated
            // (returned for the caller to satisfy); in auto mode it cannot happen
            // (the common dir contains every path), so treat it as an error.
            if root.is_some() {
                external.push(PathBuf::from(path));
                continue;
            }
            return Err(StageError::LooseStage(format!(
                "dependency escapes the common root ({base}): {path}"
            )));
        };
        let dst = dest_dir.join(rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| StageError::LooseStage(format!("create {}: {e}", parent.display())))?;
        }
        if purpose == PackagingPurpose::Runtime {
            copy_runtime_asset(Path::new(path), &dst)?;
        } else {
            std::fs::copy(path, &dst).map_err(|e| {
                StageError::LooseStage(format!("copy {path} -> {}: {e}", dst.display()))
            })?;
        }
    }

    let staged_entry = source_norm.strip_prefix(&base_prefix).ok_or_else(|| {
        StageError::LooseStage(format!(
            "source {source_norm} is not under the staging root {base}"
        ))
    })?;

    Ok(LooseStageResult {
        staged_entry: PathBuf::from(staged_entry),
        dependencies: local_deps,
        external,
    })
}

/// Strip a Windows `\\?\` verbatim prefix (when present) and forward-slash the
/// result, **without** resolving the path on disk. Unlike [`canon_fwd_slash`]
/// this never calls `fs::canonicalize`, so it is safe for a destination path that
/// does not exist yet (the usdz about to be written). Used to normalize the paths
/// handed to the hand-rolled localize-and-zip packager — see
/// [`package_stage_localize_keeping_remote`] for why the verbatim prefix breaks
/// that path on Windows.
pub(crate) fn strip_verbatim_fwd_slash(p: &Path) -> Result<String, StageError> {
    let s = p
        .to_str()
        .ok_or_else(|| StageError::NonUtf8Path(p.to_path_buf()))?;
    let s = s.strip_prefix(r"\\?\").unwrap_or(s);
    Ok(s.replace('\\', "/"))
}

/// Canonicalize `p` to one real, forward-slashed absolute form: resolves the
/// real path, strips any Windows `\\?\` verbatim prefix, and replaces `\` with
/// `/` so comparisons / common-root computation match USD's own real-path
/// spelling. Used by [`stage_loose`].
pub(crate) fn canon_fwd_slash(p: &Path) -> Result<String, StageError> {
    let real = std::fs::canonicalize(p)
        .map_err(|e| StageError::LooseStage(format!("resolve {}: {e}", p.display())))?;
    let s = real
        .to_str()
        .ok_or_else(|| StageError::NonUtf8Path(real.clone()))?;
    let s = s.strip_prefix(r"\\?\").unwrap_or(s);
    Ok(s.replace('\\', "/"))
}

/// Longest common directory prefix (forward-slashed, no trailing slash) of a set
/// of absolute file paths. Compares whole path segments so that `/a/bc` and
/// `/a/bd` yield `/a`, not `/a/b`. Returns `None` for an empty input.
pub(crate) fn longest_common_dir(paths: &[String]) -> Option<String> {
    let first = paths.first()?;
    // Seed with the first path's directory segments.
    let dir_of = |p: &str| -> Vec<String> {
        let mut segs: Vec<String> = p.split('/').map(str::to_owned).collect();
        segs.pop(); // drop the file name
        segs
    };
    let mut common = dir_of(first);
    for p in &paths[1..] {
        let segs = dir_of(p);
        let keep = common
            .iter()
            .zip(segs.iter())
            .take_while(|(a, b)| a == b)
            .count();
        common.truncate(keep);
        if common.is_empty() {
            return None;
        }
    }
    Some(common.join("/"))
}
