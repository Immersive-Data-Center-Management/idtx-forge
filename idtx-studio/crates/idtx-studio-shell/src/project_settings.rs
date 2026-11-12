//! Conversion between the open [`IdtxProject`] manifest and the UI-neutral
//! [`ProjectSettingsState`] edited by the "Project ▸ Settings…" modal.
//!
//! `idtx-editor-ui` deliberately does not depend on `idtx-project`, so this
//! shell-side module owns the manifest↔state mapping and the mandatory-field
//! validation. The dialog widget itself lives in
//! [`idtx_editor_ui::project_settings`].

use std::path::{Path, PathBuf};

use idtx_editor_ui::project_settings::ProjectSettingsState;
use idtx_project::{IdtxProject, ProjectManifest};

/// Normalize a path to a forward-slash string (invariant across platforms),
/// matching the on-disk TOML form produced by `idtx-project`.
fn norm(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Collect the project-relative USD stage files directly under `subdir`
/// (`stages/`), as forward-slash strings for the dropdowns.
fn stage_files(project: &IdtxProject, subdir: &str) -> Vec<String> {
    project
        .list_assets(subdir)
        .into_iter()
        .filter(|e| !e.is_dir && e.kind.is_stage())
        .map(|e| norm(&e.rel_path))
        .collect()
}

/// Build the dialog state from the open project's manifest + asset tree.
pub(crate) fn state_from_project(project: &IdtxProject) -> ProjectSettingsState {
    let m = project.manifest();

    let opt_path = |p: &Option<PathBuf>| p.as_deref().map(norm).unwrap_or_default();

    ProjectSettingsState {
        name: m.name.clone(),
        default_stage: m.default_stage.as_deref().map(norm),

        msaa: m.build.msaa.unwrap_or(0),
        target_fps: m.build.target_fps.unwrap_or(90),

        loose_levels: m.build.loose_levels,
        embed_dev_token: m.build.embed_dev_token,

        android_jdk: opt_path(&m.build.android.jdk),
        android_sdk: opt_path(&m.build.android.sdk),
        android_ndk: opt_path(&m.build.android.ndk),
        android_texture_quality: m
            .build
            .android
            .texture_quality
            .map(|q| q.as_token().to_owned())
            .unwrap_or_default(),

        visionos_identity: m.build.visionos.identity.clone().unwrap_or_default(),
        visionos_provisioning_profile: opt_path(&m.build.visionos.provisioning_profile),
        visionos_bundle_id: m.build.visionos.bundle_id.clone().unwrap_or_default(),
        visionos_team_id: m.build.visionos.team_id.clone().unwrap_or_default(),
        visionos_texture_quality: m
            .build
            .visionos
            .texture_quality
            .map(|q| q.as_token().to_owned())
            .unwrap_or_default(),

        // `None` = use the built-in player; a set value is a project-relative
        // `stageKind = "player"` stage.
        player_stage: m.player_stage.as_deref().map(norm),

        // Level transition: project-relative start/transfer level + the
        // shared sublayers injected into every per-level assembly. Per-level
        // spawn is authored in the level stage, not here.
        start_level: m.start_level.as_deref().map(norm),
        transfer_level: m.transfer_level.as_deref().map(norm),
        shared_sublayers: m.shared_sublayers.iter().map(|p| norm(p)).collect(),
        new_shared_sublayer: String::new(),

        network_allow: m.network.allow,
        network_allowed_hosts: m.network.allowed_hosts.join("\n"),
        network_private_hosts: m.network.private_hosts.join("\n"),
        network_auth_origins: m.network.auth_origins.join("\n"),
        network_max_response_mb: m.network.max_response_mb,
        network_max_asset_mb: m.network.max_asset_mb,

        stage_options: stage_files(project, "stages"),
        validation_errors: Vec::new(),

        // Level rows + authoring flag are refreshed by the shell each frame while
        // the dialog is open (see `render.rs`); seed them empty here.
        level_rows: Vec::new(),
        level_browser_state: idtx_editor_ui::level_browser::LevelBrowserState::default(),
        level_authoring: false,
    }
}

/// Turn an optional dropdown value into a non-empty `Option<PathBuf>`.
fn to_opt_path(value: Option<&String>) -> Option<PathBuf> {
    value
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// Turn a text box into a non-empty `Option<String>` (blank = unset).
fn to_opt_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Turn a text box into a non-empty `Option<PathBuf>` (blank = unset).
fn to_opt_path_str(value: &str) -> Option<PathBuf> {
    to_opt_string(value).map(PathBuf::from)
}

/// Apply the edited state onto `manifest`, or return the list of missing
/// mandatory fields (leaving `manifest` untouched) when validation fails.
///
/// # Errors
/// Returns `Err(messages)` when a mandatory field (name) is empty — the same
/// messages shown in the dialog.
pub(crate) fn apply_state_to_manifest(
    state: &ProjectSettingsState,
    manifest: &mut ProjectManifest,
) -> Result<(), Vec<String>> {
    let missing = state.missing_mandatory();
    if !missing.is_empty() {
        return Err(missing);
    }
    let auth_origins: Vec<String> = state.network_auth_origins.lines().map(str::trim)
        .filter(|s| !s.is_empty()).map(str::to_owned).collect();
    idtx_auth::AuthScopes::parse(&auth_origins).map_err(|e| vec![format!("Credential origins: {e}")])?;

    // ── General ──
    state.name.trim().clone_into(&mut manifest.name);
    manifest.default_stage = to_opt_path(state.default_stage.as_ref());

    // ── Rendering ── (0 is a valid "disabled" value per the dialog spec)
    manifest.build.msaa = Some(state.msaa);
    manifest.build.target_fps = Some(state.target_fps);

    // ── Cooking / Android ──
    manifest.build.loose_levels = state.loose_levels;
    manifest.build.embed_dev_token = state.embed_dev_token;
    manifest.build.android.jdk = to_opt_path_str(&state.android_jdk);
    manifest.build.android.sdk = to_opt_path_str(&state.android_sdk);
    manifest.build.android.ndk = to_opt_path_str(&state.android_ndk);
    manifest.build.android.texture_quality =
        idtx_project::TextureQuality::from_token(&state.android_texture_quality);

    // ── Cooking / VisionOS ──
    manifest.build.visionos.identity = to_opt_string(&state.visionos_identity);
    manifest.build.visionos.provisioning_profile =
        to_opt_path_str(&state.visionos_provisioning_profile);
    manifest.build.visionos.bundle_id = to_opt_string(&state.visionos_bundle_id);
    manifest.build.visionos.team_id = to_opt_string(&state.visionos_team_id);
    manifest.build.visionos.texture_quality =
        idtx_project::TextureQuality::from_token(&state.visionos_texture_quality);

    // ── Player ── blank = built-in default (unset).
    manifest.player_stage = to_opt_path(state.player_stage.as_ref());

    // ── Level transition ── blank/none → unset; empty list → clean manifest.
    // Order is preserved and blanks dropped; the add-row widget already rejects
    // duplicates, so no extra dedup is needed here.
    manifest.start_level = to_opt_path(state.start_level.as_ref());
    manifest.transfer_level = to_opt_path(state.transfer_level.as_ref());
    manifest.shared_sublayers = state
        .shared_sublayers
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();

    // ── Network ── parse the newline-separated host list; blank = deny-all.
    manifest.network.allow = state.network_allow;
    manifest.network.allowed_hosts = state
        .network_allowed_hosts
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    manifest.network.max_response_mb = state.network_max_response_mb;
    manifest.network.max_asset_mb = state.network_max_asset_mb;
    manifest.network.private_hosts = state.network_private_hosts.lines().map(str::trim)
        .filter(|s| !s.is_empty()).map(str::to_owned).collect();
    manifest.network.auth_origins = auth_origins;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_state() -> ProjectSettingsState {
        ProjectSettingsState {
            name: "Demo".to_owned(),
            default_stage: Some("stages/level1.usda".to_owned()),
            msaa: 0,
            target_fps: 72,
            android_jdk: "C:/jdk-17".to_owned(),
            player_stage: Some("stages/player.usda".to_owned()),
            ..ProjectSettingsState::default()
        }
    }

    #[test]
    fn apply_writes_scalar_and_player_fields() {
        let mut m = ProjectManifest::new("old");
        apply_state_to_manifest(&valid_state(), &mut m).unwrap();
        assert_eq!(m.name, "Demo");
        assert_eq!(m.default_stage, Some(PathBuf::from("stages/level1.usda")));
        assert_eq!(m.build.msaa, Some(0));
        assert_eq!(m.build.target_fps, Some(72));
        assert_eq!(m.build.android.jdk, Some(PathBuf::from("C:/jdk-17")));
        assert_eq!(m.player_stage, Some(PathBuf::from("stages/player.usda")));
    }

    #[test]
    fn apply_writes_network_fields() {
        let mut state = valid_state();
        state.network_allow = true;
        state.network_allowed_hosts = "api.example.com\n*.data.org\n".to_owned();
        state.network_auth_origins = "https://api.example.com:8443\nhttps://*.data.org\n".into();
        state.network_max_response_mb = 4;
        state.network_private_hosts = " api.example.com\n\n".into();
        state.network_max_asset_mb = 64;
        let mut m = ProjectManifest::new("x");
        apply_state_to_manifest(&state, &mut m).unwrap();
        assert!(m.network.allow);
        assert_eq!(
            m.network.allowed_hosts,
            vec!["api.example.com", "*.data.org"]
        );
        assert_eq!(m.network.max_response_mb, 4);
        assert_eq!(m.network.private_hosts, ["api.example.com"]);
        assert_eq!(m.network.max_asset_mb, 64);
        assert_eq!(m.network.auth_origins, ["https://api.example.com:8443", "https://*.data.org"]);
    }

    #[test]
    fn texture_quality_maps_both_ways_for_device_targets() {
        use idtx_project::TextureQuality;
        let mut state = valid_state();
        state.android_texture_quality = "full_resolution".to_owned();
        state.visionos_texture_quality = "inspector".to_owned();
        let mut m = ProjectManifest::new("x");
        apply_state_to_manifest(&state, &mut m).unwrap();
        assert_eq!(
            m.build.android.texture_quality,
            Some(TextureQuality::FullResolution)
        );
        assert_eq!(
            m.build.visionos.texture_quality,
            Some(TextureQuality::Inspector)
        );
        // A blank selection clears the override (device default applies at cook).
        let mut cleared = valid_state();
        cleared.android_texture_quality = String::new();
        let mut m2 = ProjectManifest::new("x");
        apply_state_to_manifest(&cleared, &mut m2).unwrap();
        assert_eq!(m2.build.android.texture_quality, None);
        // Round-trip from a manifest back into the dialog state.
        let project = idtx_project::IdtxProject::create(
            &std::env::temp_dir().join(format!("idtx-texq-{}", std::process::id())),
            m,
        )
        .unwrap();
        let back = state_from_project(&project);
        assert_eq!(back.android_texture_quality, "full_resolution");
        assert_eq!(back.visionos_texture_quality, "inspector");
    }

    #[test]
    fn invalid_credential_origins_do_not_partially_change_project_settings() {
        let mut state = valid_state();
        state.network_auth_origins = "https://api.example.com/path".into();
        let mut manifest = ProjectManifest::new("keep");
        assert!(apply_state_to_manifest(&state, &mut manifest).is_err());
        assert_eq!(manifest.name, "keep");
        assert!(manifest.network.auth_origins.is_empty());
    }

    #[test]
    fn apply_strips_blank_network_host_lines() {
        let mut state = valid_state();
        state.network_allow = false;
        state.network_allowed_hosts = "  \napi.example.com\n  \n".to_owned();
        let mut m = ProjectManifest::new("x");
        apply_state_to_manifest(&state, &mut m).unwrap();
        // allow is false but host list still parsed (deny-all despite hosts).
        assert!(!m.network.allow);
        assert_eq!(m.network.allowed_hosts, vec!["api.example.com"]);
    }

    #[test]
    fn apply_rejects_missing_name() {
        let mut state = valid_state();
        state.name = "  ".to_owned();
        let mut m = ProjectManifest::new("keep");
        let err = apply_state_to_manifest(&state, &mut m).unwrap_err();
        assert_eq!(err.len(), 1);
        // Manifest left untouched on failure.
        assert_eq!(m.name, "keep");
    }

    #[test]
    fn blank_optionals_become_none() {
        let mut state = valid_state();
        state.default_stage = Some("   ".to_owned());
        state.android_jdk = String::new();
        state.visionos_identity = String::new();
        state.player_stage = Some("  ".to_owned());
        let mut m = ProjectManifest::new("x");
        apply_state_to_manifest(&state, &mut m).unwrap();
        assert_eq!(m.default_stage, None);
        assert_eq!(m.build.android.jdk, None);
        assert_eq!(m.build.visionos.identity, None);
        assert_eq!(m.player_stage, None, "blank player stage = built-in default");
    }

    #[test]
    fn unset_player_stage_uses_builtin() {
        let mut state = valid_state();
        state.player_stage = None;
        let mut m = ProjectManifest::new("x");
        apply_state_to_manifest(&state, &mut m).unwrap();
        assert_eq!(m.player_stage, None);
    }

    #[test]
    fn apply_writes_transition_fields() {
        let mut state = valid_state();
        state.start_level = Some("stages/forest.usda".to_owned());
        state.transfer_level = Some("stages/_transfer.usda".to_owned());
        state.shared_sublayers = vec![
            "stages/_globals/a.usda".to_owned(),
            "stages/_globals/b.usda".to_owned(),
        ];
        let mut m = ProjectManifest::new("x");
        apply_state_to_manifest(&state, &mut m).unwrap();
        assert_eq!(m.start_level, Some(PathBuf::from("stages/forest.usda")));
        assert_eq!(m.transfer_level, Some(PathBuf::from("stages/_transfer.usda")));
        assert_eq!(
            m.shared_sublayers,
            vec![
                PathBuf::from("stages/_globals/a.usda"),
                PathBuf::from("stages/_globals/b.usda"),
            ],
            "order preserved"
        );
    }

    #[test]
    fn blank_transition_fields_become_none_and_empty() {
        let mut state = valid_state();
        state.start_level = Some("   ".to_owned());
        state.transfer_level = None;
        // Blank entries are dropped; an all-blank list collapses to empty.
        state.shared_sublayers = vec![String::new(), "  ".to_owned()];
        let mut m = ProjectManifest::new("x");
        apply_state_to_manifest(&state, &mut m).unwrap();
        assert_eq!(m.start_level, None, "blank start level = unset");
        assert_eq!(m.transfer_level, None, "unset transfer level stays None");
        assert!(
            m.shared_sublayers.is_empty(),
            "blank-only shared list → clean manifest"
        );
    }

    #[test]
    fn state_from_project_round_trips_transition_fields() {
        // manifest → state → manifest preserves the transition fields.
        let dir = scratch("transition");
        let mut manifest = ProjectManifest::new("Trans");
        manifest.start_level = Some(PathBuf::from("stages/forest.usda"));
        manifest.transfer_level = Some(PathBuf::from("stages/_transfer.usda"));
        manifest.shared_sublayers = vec![PathBuf::from("stages/_globals/prototypes.usda")];
        let project = IdtxProject::create(&dir, manifest).unwrap();

        let state = state_from_project(&project);
        assert_eq!(state.start_level.as_deref(), Some("stages/forest.usda"));
        assert_eq!(state.transfer_level.as_deref(), Some("stages/_transfer.usda"));
        assert_eq!(state.shared_sublayers, vec!["stages/_globals/prototypes.usda"]);
        assert!(state.new_shared_sublayer.is_empty());

        let mut out = ProjectManifest::new("x");
        apply_state_to_manifest(&state, &mut out).unwrap();
        assert_eq!(out.start_level, Some(PathBuf::from("stages/forest.usda")));
        assert_eq!(out.transfer_level, Some(PathBuf::from("stages/_transfer.usda")));
        assert_eq!(
            out.shared_sublayers,
            vec![PathBuf::from("stages/_globals/prototypes.usda")]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Unique scratch dir under the system temp dir (no `tempfile` dep).
    fn scratch(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "idtx-projsettings-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
