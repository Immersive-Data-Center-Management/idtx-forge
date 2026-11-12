//! Built-in assets shipped with the toolbox.
//!
//! The crate owns the **built-in USD tree** — a canonical player stage, its
//! desktop/XR rigs, and the default gameplay behavior sources — that project
//! authoring can reference via a `built-in/<path>/<stage>` layer reference. The
//! reference resolves because [`builtin_root`] is placed on the USD default
//! asset-search path (`ArDefaultResolver::SetDefaultSearchPath`): the resolver
//! then finds `<search-dir>/built-in/players/Player.usd` and friends.
//!
//! The tree lives at `crates/idtx-assets/built-in/` in the workspace and is
//! staged **beside the executable** when the Conductor is bundled or a runtime
//! is cooked. [`builtin_root`] returns the directory that *contains* the
//! `built-in/` folder (the search dir), discovering it for both layouts.

use std::path::{Path, PathBuf};

/// The on-disk name of the built-in tree directory. Also the leading path
/// segment of every `built-in/...` layer reference, so the search dir handed to
/// the resolver must be the directory that *contains* this folder.
pub const BUILTIN_DIR_NAME: &str = "built-in";

/// The canonical `built-in/...` reference arc for the built-in player stage —
/// the default a project's `player_stage` resolves to when unset.
pub const BUILTIN_PLAYER_STAGE_REF: &str = "built-in/players/Player.usd";

/// Environment override naming the directory that contains the `built-in/`
/// tree (the search dir, not the tree itself). Set in tests and when the tree
/// has been relocated away from the executable.
pub const ENV_BUILTIN_DIR: &str = "IDTX_BUILTIN_DIR";

/// Whether `dir` is a plausible built-in **search dir**: it exists and holds a
/// `built-in/` subdirectory.
fn has_builtin(dir: &std::path::Path) -> bool {
    dir.join(BUILTIN_DIR_NAME).is_dir()
}

/// The directory that contains the `built-in/` tree — the value to put on the
/// USD default asset-search path so `@built-in/...@` references resolve.
///
/// Discovery order:
/// 1. [`ENV_BUILTIN_DIR`] if it names a dir that contains `built-in/`.
/// 2. The executable's directory (`<exe>/built-in/`), the packaged layout.
/// 3. This crate's manifest dir (`crates/idtx-assets/built-in/`), the dev
///    layout, else walking up the ancestor chain for a
///    `crates/idtx-assets/built-in` tree (so a nested cwd still finds it).
///
/// Falls back to the crate manifest dir (branch 3's anchor) even when no
/// `built-in/` is found, so the returned path is always well-formed; callers
/// that must have real files should check [`builtin_tree`] exists.
#[must_use]
pub fn builtin_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    // Priority-ordered candidate search dirs:
    // 1. explicit override, 2. the executable's dir (packaged), 3. the crate's
    // own dir (dev). The first that actually contains `built-in/` wins.
    let env_dir = std::env::var_os(ENV_BUILTIN_DIR).map(PathBuf::from);
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let candidates = env_dir
        .into_iter()
        .chain(exe_dir)
        .chain(std::iter::once(manifest_dir.clone()));
    for dir in candidates {
        if has_builtin(&dir) {
            return dir;
        }
    }

    // Dev fallback: walk up for a `crates/idtx-assets/built-in` tree (nested cwd).
    for ancestor in manifest_dir.ancestors() {
        let candidate = ancestor.join("crates").join("idtx-assets");
        if has_builtin(&candidate) {
            return candidate;
        }
    }

    // Nothing found — return the crate dir so the path is at least well-formed.
    manifest_dir
}

/// The absolute path of the `built-in/` tree itself (`builtin_root()/built-in`).
#[must_use]
pub fn builtin_tree() -> PathBuf {
    builtin_root().join(BUILTIN_DIR_NAME)
}

/// The absolute path of the built-in player stage
/// (`builtin_tree()/players/Player.usd`).
#[must_use]
pub fn builtin_player_stage() -> PathBuf {
    builtin_tree().join("players").join("Player.usd")
}

/// The subdirectory of the built-in tree holding the toolbox's shipped XR
/// controller meshes (`generic`, `questpro`, `quest2`).
pub const BUILTIN_CONTROLLERS_DIR_NAME: &str = "controllers";

/// The absolute path of the built-in XR controller-mesh directory
/// (`builtin_tree()/controllers`).
///
/// Holds the toolbox's shipped `generic` / `questpro` / `quest2` controller
/// meshes. The cook resolves each `run.toml` `controllers/<file>` arc against
/// this directory's **parent** ([`builtin_tree`]) as a lowest-priority fallback
/// root, so a project that declares no local controller meshes still ships them
/// in the cooked payload and the packaged runtime finds a mesh for every device.
#[must_use]
pub fn builtin_controllers_dir() -> PathBuf {
    builtin_tree().join(BUILTIN_CONTROLLERS_DIR_NAME)
}

/// The subdirectory of the built-in tree holding the toolbox's shipped text
/// fonts (`built-in/fonts/`).
pub const BUILTIN_FONTS_DIR_NAME: &str = "fonts";

/// Family name of the default built-in text font. Widget text whose
/// `idtx:widget:fontFamily` is unset (or names an unregistered family) renders
/// in this family. Matches the family registered from [`default_font_bytes`].
pub const DEFAULT_FONT_FAMILY: &str = "Roboto";

/// On-disk filename of the default built-in font inside [`builtin_fonts_dir`].
pub const DEFAULT_FONT_FILE: &str = "Roboto-Regular.ttf";

/// The absolute path of the built-in font directory (`builtin_tree()/fonts`).
///
/// Holds the toolbox's shipped text fonts. The file bytes are *also* compiled
/// into the binary via [`default_font_bytes`], so the renderer has a guaranteed
/// default font without touching the filesystem; this path exists for cook
/// staging and resolver-anchored lookups of additional fonts.
#[must_use]
pub fn builtin_fonts_dir() -> PathBuf {
    builtin_tree().join(BUILTIN_FONTS_DIR_NAME)
}

/// The raw bytes of the default built-in font (`Roboto-Regular.ttf`,
/// Apache-2.0), compiled into the binary.
///
/// Returned as a `'static` slice so render consumers can build a font atlas
/// unconditionally — in tests, the Conductor preview, and the packaged runtime
/// alike — with no dependency on a staged `built-in/fonts/` tree or the USD
/// asset resolver. The on-disk copy under [`builtin_fonts_dir`] is the font's
/// stable home for cook staging and future resolver-loaded families.
#[must_use]
pub fn default_font_bytes() -> &'static [u8] {
    include_bytes!("../built-in/fonts/Roboto-Regular.ttf")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch dir holding a `built-in/players/Player.usd` skeleton.
    fn scratch_with_tree(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "idtx-builtin-{tag}-{}-{nanos}",
            std::process::id()
        ));
        let players = dir.join(BUILTIN_DIR_NAME).join("players");
        std::fs::create_dir_all(&players).unwrap();
        std::fs::write(players.join("Player.usd"), b"#usda 1.0\n").unwrap();
        dir
    }

    #[test]
    fn env_override_wins_when_it_has_the_tree() {
        let dir = scratch_with_tree("env");
        // SAFETY: single-threaded test; restore after.
        unsafe { std::env::set_var(ENV_BUILTIN_DIR, &dir) };
        let root = builtin_root();
        unsafe { std::env::remove_var(ENV_BUILTIN_DIR) };
        assert_eq!(
            root.canonicalize().unwrap(),
            dir.canonicalize().unwrap(),
            "env override must be returned when it contains built-in/"
        );
        assert!(root.join(BUILTIN_DIR_NAME).join("players").join("Player.usd").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn env_override_ignored_when_tree_absent() {
        let empty = std::env::temp_dir().join(format!("idtx-builtin-empty-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        unsafe { std::env::set_var(ENV_BUILTIN_DIR, &empty) };
        let root = builtin_root();
        unsafe { std::env::remove_var(ENV_BUILTIN_DIR) };
        assert_ne!(
            root, empty,
            "an override without built-in/ must fall through to discovery"
        );
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn dev_discovery_finds_the_workspace_tree() {
        // In the workspace, the crate ships the real tree; builtin_tree() must
        // resolve to a real directory with the player stage.
        assert!(
            builtin_tree().is_dir(),
            "built-in tree missing at {:?}",
            builtin_tree()
        );
        assert!(
            builtin_player_stage().is_file(),
            "built-in player stage missing at {:?}",
            builtin_player_stage()
        );
    }

    #[test]
    fn builtin_controllers_dir_ships_the_device_meshes() {
        // The cook resolves `controllers/<file>` arcs against builtin_tree() as a
        // fallback root, so the shipped meshes must live under
        // `built-in/controllers/`. Guards against the meshes being moved/renamed
        // out from under the cook's fallback resolution.
        let dir = builtin_controllers_dir();
        assert!(dir.is_dir(), "built-in controllers dir missing at {dir:?}");
        for mesh in [
            "generic_left.usda",
            "generic_right.usda",
            "questpro_left.usdz",
            "questpro_right.usdz",
            "quest2_left.usdz",
            "quest2_right.usdz",
        ] {
            assert!(
                dir.join(mesh).is_file(),
                "shipped controller mesh missing: {mesh}"
            );
        }
        // The arcs in run.toml are `controllers/<file>`, so they must resolve
        // against the *parent* (builtin_tree()), not the controllers dir itself.
        assert_eq!(dir.parent(), Some(builtin_tree().as_path()));
    }

    #[test]
    fn builtin_fonts_dir_ships_the_default_font() {
        // The default widget-text font (Roboto-Regular, Apache-2.0) lives in
        // `built-in/fonts/` so the cook stages it and resolver lookups of extra
        // families have a home. Its bytes are also compiled in via
        // default_font_bytes(); guard both against the font being moved/renamed.
        let dir = builtin_fonts_dir();
        assert!(dir.is_dir(), "built-in fonts dir missing at {dir:?}");
        assert!(
            dir.join(DEFAULT_FONT_FILE).is_file(),
            "default font missing: {DEFAULT_FONT_FILE}"
        );
        assert!(
            dir.join("Roboto-LICENSE.txt").is_file(),
            "font license file missing beside the font"
        );
        assert_eq!(dir.parent(), Some(builtin_tree().as_path()));

        // The compiled-in copy is a non-empty TrueType blob (starts with the
        // `0x00010000` sfnt version tag used by .ttf).
        let bytes = default_font_bytes();
        assert!(bytes.len() > 10_000, "embedded font suspiciously small: {}", bytes.len());
        assert_eq!(&bytes[0..4], &[0x00, 0x01, 0x00, 0x00], "not a TrueType sfnt blob");
    }
}
