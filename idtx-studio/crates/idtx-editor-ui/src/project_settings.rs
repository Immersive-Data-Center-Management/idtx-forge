// Project Settings modal — edits the Conductor project manifest (`project.toml`).
//
// This is a *pure egui* widget: it carries plain-field editable state and emits
// intents, exactly like [`app_config_window`](crate::remote_browser::app_config_window).
// It never touches `idtx-project` — the shell owns the manifest↔state conversion,
// validation, native path pickers, and the save (so this crate stays UI-neutral
// and layering-clean, mirroring the asset/level browsers).

/// Which path field the user asked to pick via a native dialog. The shell runs
/// the picker (`rfd`) in its action-apply pass and writes the result back into
/// the matching field of the live [`ProjectSettingsState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathField {
    /// `[build.android] jdk`.
    Jdk,
    /// `[build.android] sdk`.
    Sdk,
    /// `[build.android] ndk`.
    Ndk,
    /// `[build.visionos] provisioning_profile` (a `*.mobileprovision` file).
    ProvisioningProfile,
}

/// One editable row of the `[runtime_prototypes]` table — a stable `key`
/// (the TOML sub-table name, which must be a unique ASCII identifier) plus the
/// two columns of [`idtx_runtime_manifest::RuntimePrototypeConfig`]: the
/// project-relative `asset` USD file and the absolute `prim` path inside it.
///
/// Edited as an ordered `Vec` rather than a map so the user can type a key in
/// place (a map keyed on a half-typed identifier would thrash); the shell folds
/// the rows back into the manifest's `BTreeMap` on save, rejecting duplicate or
/// empty keys so the stored table keeps unique keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimePrototypeRow {
    /// The prototype id — the `[runtime_prototypes.<key>]` sub-table name. Must
    /// be a non-empty ASCII identifier, unique across all rows.
    pub key: String,
    /// Project-relative USD asset path (`/`-separated), e.g. `assets/panel.usda`.
    pub asset: String,
    /// Absolute prim path inside the asset, e.g. `/Panel`.
    pub prim: String,
    /// Root-level prim paths (`/`-prefixed) discovered in the selected `asset`,
    /// offered as the prim dropdown. **Shell-owned**: the pure widget never reads
    /// USD; the shell refreshes this each frame from the asset's root layer (empty
    /// when no asset is selected or the file can't be read). Not persisted.
    pub prim_options: Vec<String>,
}

/// Intent emitted by [`project_settings_window`].
#[derive(Debug, Clone)]
pub enum ProjectSettingsAction {
    /// User clicked "Save" and all mandatory fields were present. The shell
    /// applies the state to the manifest and persists `project.toml`.
    Save,
    /// User cancelled (button, Escape, or window close) — discard all edits.
    Cancel,
    /// User asked to pick a path with a native dialog for `field`.
    PickPath {
        /// The field whose value the picker fills in.
        field: PathField,
    },
    /// A level-list interaction in the "Levels" section. The shell maps this onto
    /// its own level command bus; it never closes the dialog.
    Level(crate::level_browser::LevelBrowserAction),
}

/// Valid MSAA sample counts offered in the Rendering section. `0` disables MSAA.
pub const MSAA_OPTIONS: &[u32] = &[0, 1, 2, 4];

/// Texture-quality choices offered for the device (Android / VisionOS) cooking
/// sections, as `(stored token, display label)`. The empty token means "device
/// default" — the cook picks the capped `authoring` profile. The non-empty
/// tokens round-trip through `idtx_project::TextureQuality` /
/// `idtx_render::TextureDecodeConfig::from_profile_token` into the cooked
/// `run.toml`.
pub const TEXTURE_QUALITY_OPTIONS: &[(&str, &str)] = &[
    ("", "Default — High, 2048 cap (recommended)"),
    ("full_resolution", "Full resolution — no cap (may exhaust device memory)"),
    ("authoring", "High — 2048 cap"),
    ("inspector", "Balanced — 1024 cap"),
];

/// Editable, UI-neutral mirror of the parts of `ProjectManifest` the Settings
/// dialog maintains. The shell builds this from the open project and applies it
/// back on Save; see `idtx_studio_shell::project_settings`.
#[derive(Debug, Clone, Default)]
pub struct ProjectSettingsState {
    // ── General ──────────────────────────────────────────────────────────────
    /// Project display name (`name`). **Mandatory** (`String`, not `Option`).
    pub name: String,
    /// Default stage, project-relative with `/` separators (`default_stage`).
    /// `None` = "open no stage". Optional.
    pub default_stage: Option<String>,

    // ── Rendering ──────────────────────────────────────────────────────────────
    /// `build.msaa` sample count; `0` means disabled. Optional.
    pub msaa: u32,
    /// `build.target_fps`; defaults to 90. Optional.
    pub target_fps: u32,

    // ── Cooking / Android (`[build.android]`) ──────────────────────────────────
    /// `build.loose_levels`: keep large referenced scenes as loose files (levels
    /// as on-demand payloads) instead of baking the whole closure into one
    /// `stage.usdz`. Faster to load, larger packaged folder.
    pub loose_levels: bool,
    /// ⚠️ TEMPORARY / security risk — `build.embed_dev_token`: embed the
    /// logged-in bearer token into the cooked `run.toml` so a device build can
    /// authenticate remote USD fetches (no keychain on device). The build carries
    /// credentials and must not be distributed. Default off.
    pub embed_dev_token: bool,
    /// `jdk` root; empty = unset. Optional.
    pub android_jdk: String,
    /// `sdk` root; empty = unset. Optional.
    pub android_sdk: String,
    /// `ndk` root; empty = unset. Optional.
    pub android_ndk: String,
    /// `[build.android].texture_quality` token (`"full_resolution"` /
    /// `"authoring"` / `"inspector"`); empty = use the device default (capped).
    /// Governs how textures are resolution-capped on the cooked Quest build.
    pub android_texture_quality: String,

    // ── Cooking / VisionOS (`[build.visionos]`) ────────────────────────────────
    /// `identity`; empty = unset. Optional.
    pub visionos_identity: String,
    /// `provisioning_profile` path; empty = unset. Optional.
    pub visionos_provisioning_profile: String,
    /// `bundle_id`; empty = unset. Optional.
    pub visionos_bundle_id: String,
    /// `team_id`; empty = unset. Optional.
    pub visionos_team_id: String,
    /// `[build.visionos].texture_quality` token (`"full_resolution"` /
    /// `"authoring"` / `"inspector"`); empty = use the device default (capped).
    /// Governs how textures are resolution-capped on the cooked Vision Pro build.
    pub visionos_texture_quality: String,

    // ── Player (`player_stage`) ────────────────────────────────────────────────
    /// The project's player stage (project-relative, `stageKind = "player"`).
    /// `None` = use the toolbox's built-in player. Optional.
    pub player_stage: Option<String>,

    // ── Dropdown sources + feedback (filled by the shell / this widget) ─────────
    /// USD stage files under `stages/` (project-relative), for stage dropdowns.
    pub stage_options: Vec<String>,
    /// Mandatory-field messages shown after a failed Save; empty otherwise.
    pub validation_errors: Vec<String>,

    // ── Levels (`[[levels]]`) ───────────────────────────────────────────────────
    /// Registered level rows, rebuilt by the shell each frame while the dialog
    /// is open (so `is_open` tracks the active tab).
    pub level_rows: Vec<crate::level_browser::LevelRow>,
    /// Persistent selection state for the Levels list (survives across frames).
    pub level_browser_state: crate::level_browser::LevelBrowserState,
    /// Whether level create/add/remove/reorder controls are offered (authoring
    /// product with a project open).
    pub level_authoring: bool,

    // ── Level transition (`start_level` / `transfer_level` / `shared_sublayers`) ─
    /// The level opened + played at boot (`start_level`), project-relative with
    /// `/` separators. `None` = fall back to `default_stage` → first level.
    /// Optional.
    pub start_level: Option<String>,
    /// The transition "mask" scene shown while a level swap loads
    /// (`transfer_level`), project-relative with `/` separators. `None` = no
    /// dedicated transfer scene. Optional.
    pub transfer_level: Option<String>,
    /// Shared sublayers injected into every per-level assembly
    /// (`shared_sublayers`), each project-relative with `/` separators. Order is
    /// preserved; blanks and duplicates are dropped on save.
    pub shared_sublayers: Vec<String>,
    /// Scratch text for the "add a shared sublayer" row; not persisted.
    pub new_shared_sublayer: String,
    // NOTE: per-level spawn is intentionally NOT surfaced here — there is no
    // project-manifest field for it. A level's spawn is authored in that level's
    // own USD stage at `/World/level/Spawn` (the behavior passes a spawn id in
    // its transition request); project settings only pick levels, not spawns.

    // ── Network (`[network]`) ───────────────────────────────────────────────────
    /// Master switch for stage-authored `OpenExec` REST datasources. `false` =
    /// deny all polling (the safe default).
    pub network_allow: bool,
    /// Newline-separated list of allowed endpoint host patterns. Each line is
    /// an exact host (`api.example.com`) or a `*.domain` wildcard. Blank lines
    /// and leading/trailing whitespace are ignored on save.
    pub network_allowed_hosts: String,
    /// Explicit private-network host patterns, one per line.
    pub network_private_hosts: String,
    /// Newline-separated credential origins, including scheme and optional port.
    pub network_auth_origins: String,
    /// Maximum accepted REST response body size, in megabytes (MiB). `0` uses the
    /// runtime default (1 MiB); a larger response is dropped so a hostile/runaway
    /// endpoint cannot exhaust memory.
    pub network_max_response_mb: u32,
    /// Streamed asset cap in MiB; zero selects 256 MiB.
    pub network_max_asset_mb: u32,

    // ── Behavior (`[behavior.limits]`) ──────────────────────────────────────────
    /// Per-instance deferred-work byte budget (session edits, overlays, async HTTP
    /// response reservations). `0` uses the runtime default (2 MiB).
    pub behavior_queue_bytes: usize,
    /// Single async-HTTP response cap + up-front reservation. `0` uses the runtime
    /// default (1 MiB); also clamped by the network max-response size.
    pub behavior_http_response_bytes: usize,
    /// Max concurrent in-flight async HTTP requests per behavior instance. `0` uses
    /// the runtime default (8).
    pub behavior_http_inflight: usize,

    // ── Runtime prototypes (`[runtime_prototypes]`) ─────────────────────────────
    /// Source USD spawn templates prepared even when no live instance exists.
    /// Edited as ordered rows; the shell folds them into the manifest's keyed
    /// table on save, rejecting blank or duplicate keys so each `key` is unique.
    pub runtime_prototypes: Vec<RuntimePrototypeRow>,
    /// Project-relative USD files (`/`-separated) offered in each prototype's
    /// asset dropdown. **Shell-owned**: refreshed each frame from the project's
    /// asset tree (the pure widget never touches the filesystem).
    pub asset_options: Vec<String>,
}

impl ProjectSettingsState {
    /// The mandatory fields that are currently empty, as human-readable messages.
    /// Mandatory = the manifest field is not `Option` (`name`). The player stage
    /// is optional (unset = the built-in player). An empty result means Save may
    /// proceed.
    #[must_use]
    pub fn missing_mandatory(&self) -> Vec<String> {
        let mut missing = Vec::new();
        if self.name.trim().is_empty() {
            missing.push("General: Name is required.".to_owned());
        }
        missing
    }
}

/// A field label followed by a red mandatory asterisk.
fn mandatory_label(ui: &mut egui::Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.label(text);
        ui.label(egui::RichText::new("*").color(crate::theme::current().status_err));
    });
}

/// A dropdown that selects an optional project-relative path from `options`,
/// with a leading `(none)` entry. `id_salt` disambiguates the combo box.
fn optional_path_combo(
    ui: &mut egui::Ui,
    id_salt: &str,
    value: &mut Option<String>,
    options: &[String],
) {
    let selected_text = value.clone().unwrap_or_else(|| "(none)".to_owned());
    egui::ComboBox::from_id_salt(id_salt)
        .selected_text(selected_text)
        .width(300.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(value, None, "(none)");
            for opt in options {
                ui.selectable_value(value, Some(opt.clone()), opt);
            }
        });
}

/// A non-fatal warning shown when a selected transition stage is not among the
/// project's registered stage options (e.g. the file was moved or removed). The
/// value is still kept; this just flags that it won't resolve.
fn transition_stage_note(ui: &mut egui::Ui, value: Option<&String>, options: &[String]) {
    if let Some(v) = value {
        let v = v.trim();
        if !v.is_empty() && !options.iter().any(|o| o == v) {
            ui.label(
                egui::RichText::new(format!("⚠ \"{v}\" is not a registered stage."))
                    .color(crate::theme::current().status_err)
                    .italics(),
            );
        }
    }
}

/// A locomotion dropdown (reserved for a future per-player locomotion setting).
#[allow(dead_code)]
fn locomotion_combo(ui: &mut egui::Ui, id_salt: &str, value: &mut String) {
    egui::ComboBox::from_id_salt(id_salt)
        .selected_text(value.clone())
        .show_ui(ui, |ui| {
            ui.selectable_value(value, "smooth".to_owned(), "smooth");
        });
}

/// A texture-quality dropdown for the device cooking sections. `value` holds the
/// stored token (empty = device default); the shown text is the matching label.
fn texture_quality_combo(ui: &mut egui::Ui, id_salt: &str, value: &mut String) {
    let selected = TEXTURE_QUALITY_OPTIONS
        .iter()
        .find(|(tok, _)| tok == value)
        .map_or(TEXTURE_QUALITY_OPTIONS[0].1, |(_, label)| *label);
    egui::ComboBox::from_id_salt(id_salt)
        .selected_text(selected)
        .show_ui(ui, |ui| {
            for (tok, label) in TEXTURE_QUALITY_OPTIONS {
                ui.selectable_value(value, (*tok).to_owned(), *label);
            }
        });
}

/// A path text field with a trailing "Browse…" button; the button emits a
/// [`PathField`] pick request (set into `*pick`).
fn path_picker_row(
    ui: &mut egui::Ui,
    value: &mut String,
    field: PathField,
    pick: &mut Option<PathField>,
) {
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(value).desired_width(300.0));
        if ui.button("Browse…").clicked() {
            *pick = Some(field);
        }
    });
}

/// Render the editable `[runtime_prototypes]` table: one block per spawn
/// template, each with its `key`, `asset`, and `prim` columns, plus add/remove
/// controls.
///
/// - **Asset** is an editable dropdown of the project's USD files
///   (`asset_options`, shell-supplied) alongside a free-text field for a path
///   not in the list.
/// - **Prim** is an editable dropdown of the asset's root-level prims
///   (`row.prim_options`, shell-supplied from the selected file's root layer)
///   alongside a free-text field; a typed prim whose first path segment is not a
///   root prim of the asset is flagged inline (non-fatal — nested or not-yet-
///   authored prims are still allowed).
///
/// Keys that collide with another row are tinted red (the shell also hard-rejects
/// duplicate/blank keys on save, so the stored table always has unique keys).
fn runtime_prototypes_section(
    ui: &mut egui::Ui,
    rows: &mut Vec<RuntimePrototypeRow>,
    asset_options: &[String],
) {
    ui.label(
        egui::RichText::new(
            "USD spawn templates behaviors can instantiate at runtime. Each key is a \
             unique identifier; asset is a project USD file; prim is a prim path inside it.",
        )
        .weak()
        .italics(),
    );
    ui.add_space(6.0);

    // Keys appearing on more than one non-blank row — highlighted inline so the
    // user can fix a collision before Save (which the shell rejects). Owns its
    // keys so it does not borrow `rows` across the mutable loop below.
    let mut seen: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for row in rows.iter() {
        let key = row.key.trim();
        if !key.is_empty() {
            *seen.entry(key.to_owned()).or_insert(0) += 1;
        }
    }
    let err_color = crate::theme::current().status_err;

    let mut remove_at: Option<usize> = None;
    for (i, row) in rows.iter_mut().enumerate() {
        ui.group(|ui| {
            // ── Key + Remove ──────────────────────────────────────────────
            ui.horizontal(|ui| {
                ui.label("Key");
                let dup = {
                    let key = row.key.trim();
                    !key.is_empty() && seen.get(key).copied().unwrap_or(0) > 1
                };
                let edit = egui::TextEdit::singleline(&mut row.key)
                    .hint_text("panel")
                    .desired_width(180.0);
                let edit = if dup { edit.text_color(err_color) } else { edit };
                ui.add(edit).on_hover_text(if dup {
                    "Duplicate key — must be unique."
                } else {
                    "Unique ASCII identifier (the [runtime_prototypes.<key>] name)."
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Remove").clicked() {
                        remove_at = Some(i);
                    }
                });
            });

            // ── Asset: pick from project USD files or type a path ─────────
            ui.horizontal(|ui| {
                ui.label("Asset");
                egui::ComboBox::from_id_salt(("ps_proto_asset", i))
                    .selected_text(if row.asset.is_empty() {
                        "Select USD file…".to_owned()
                    } else {
                        row.asset.clone()
                    })
                    .width(240.0)
                    .show_ui(ui, |ui| {
                        for opt in asset_options {
                            ui.selectable_value(&mut row.asset, opt.clone(), opt);
                        }
                    });
                ui.add(
                    egui::TextEdit::singleline(&mut row.asset)
                        .hint_text("assets/panel.usda")
                        .desired_width(200.0),
                );
            });

            // ── Prim: pick from the asset's root prims or type a path ─────
            ui.horizontal(|ui| {
                ui.label("Prim");
                let combo = egui::ComboBox::from_id_salt(("ps_proto_prim", i))
                    .selected_text(if row.prim.is_empty() {
                        "Select prim…".to_owned()
                    } else {
                        row.prim.clone()
                    })
                    .width(180.0);
                combo.show_ui(ui, |ui| {
                    if row.prim_options.is_empty() {
                        ui.label(
                            egui::RichText::new("(no root prims found)")
                                .weak()
                                .italics(),
                        );
                    }
                    for opt in &row.prim_options {
                        ui.selectable_value(&mut row.prim, opt.clone(), opt);
                    }
                });
                ui.add(
                    egui::TextEdit::singleline(&mut row.prim)
                        .hint_text("/Panel")
                        .desired_width(160.0),
                );
            });
            prototype_prim_note(ui, row, err_color);
        });
        ui.add_space(4.0);
    }
    if let Some(i) = remove_at {
        rows.remove(i);
    }
    ui.add_space(2.0);
    if ui.button("Add prototype").clicked() {
        rows.push(RuntimePrototypeRow::default());
    }
}

/// A non-fatal note under a prototype row when the typed `prim`'s first path
/// segment is not one of the asset's root-level prims. Shown only when we have a
/// non-empty prim AND a known root-prim list to check against (an unread / empty
/// asset can't be verified, so it stays silent). Nested paths under a real root
/// (e.g. `/World/Panel`) and not-yet-authored prims are intentionally allowed.
fn prototype_prim_note(ui: &mut egui::Ui, row: &RuntimePrototypeRow, err_color: egui::Color32) {
    let prim = row.prim.trim();
    if prim.is_empty() || row.prim_options.is_empty() {
        return;
    }
    // First path segment of the typed prim: "/World/Panel" → "World".
    let first = prim.strip_prefix('/').unwrap_or(prim).split('/').next().unwrap_or("");
    let root = format!("/{first}");
    if !row.prim_options.iter().any(|o| o == &root) {
        ui.label(
            egui::RichText::new(format!(
                "⚠ \"/{first}\" is not a root prim of the selected asset."
            ))
            .color(err_color)
            .italics(),
        );
    }
}

/// Render the "Project Settings" modal, returning an intent when the user acts.
///
/// `Save` is only returned when every mandatory field is present; otherwise the
/// missing-field messages are written into `state.validation_errors` and `None`
/// is returned so the dialog stays open. Cancel / Escape / window-close ignore
/// the mandatory checks and return [`ProjectSettingsAction::Cancel`].
#[must_use]
pub fn project_settings_window(
    ctx: &egui::Context,
    state: &mut ProjectSettingsState,
) -> Option<ProjectSettingsAction> {
    let mut action: Option<ProjectSettingsAction> = None;
    let mut pick: Option<PathField> = None;
    let mut level_action: Option<ProjectSettingsAction> = None;
    let mut open = true;

    egui::Window::new("Project Settings")
        .open(&mut open)
        .default_size([600.0, 560.0])
        .resizable(true)
        .collapsible(false)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .max_height(560.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    // ── General ──────────────────────────────────────────────
                    egui::CollapsingHeader::new("General")
                        .default_open(true)
                        .show(ui, |ui| {
                            mandatory_label(ui, "Name");
                            ui.add(
                                egui::TextEdit::singleline(&mut state.name)
                                    .hint_text("My Project")
                                    .desired_width(300.0),
                            );
                            ui.add_space(6.0);
                            ui.label("Default stage");
                            optional_path_combo(
                                ui,
                                "ps_default_stage",
                                &mut state.default_stage,
                                &state.stage_options,
                            );
                        });

                    // ── Levels ───────────────────────────────────────────────
                    egui::CollapsingHeader::new("Levels")
                        .default_open(true)
                        .show(ui, |ui| {
                            let acts = crate::level_browser::level_list_section(
                                ui,
                                &state.level_rows,
                                state.level_authoring,
                                &mut state.level_browser_state,
                            );
                            // One meaningful interaction per frame; keep the
                            // first and let the shell apply it without closing.
                            if let Some(first) = acts.into_iter().next() {
                                level_action = Some(ProjectSettingsAction::Level(first));
                            }
                        });

                    // ── Level transition ─────────────────────────────────────
                    egui::CollapsingHeader::new("Level transition")
                        .default_open(true)
                        .show(ui, |ui| {
                            ui.label("Start level");
                            optional_path_combo(
                                ui,
                                "ps_start_level",
                                &mut state.start_level,
                                &state.stage_options,
                            );
                            transition_stage_note(ui, state.start_level.as_ref(), &state.stage_options);
                            ui.add_space(6.0);

                            ui.label("Transfer level");
                            optional_path_combo(
                                ui,
                                "ps_transfer_level",
                                &mut state.transfer_level,
                                &state.stage_options,
                            );
                            transition_stage_note(
                                ui,
                                state.transfer_level.as_ref(),
                                &state.stage_options,
                            );
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(
                                    "Start level is opened + played at boot (falls back to the \
                                     default stage, then the first level). Transfer level is the \
                                     mask scene shown while a swap loads. Per-level spawn is \
                                     authored in each level stage at /World/level/Spawn, not here.",
                                )
                                .weak()
                                .italics(),
                            );

                            ui.add_space(10.0);
                            ui.strong("Shared sublayers");
                            ui.label(
                                egui::RichText::new(
                                    "Injected as sublayers into every per-level assembly.",
                                )
                                .weak()
                                .italics(),
                            );
                            ui.add_space(4.0);
                            // Existing entries, each with a Remove button. Defer the
                            // actual removal until after the loop to avoid mutating
                            // the Vec while iterating it.
                            let mut remove_at: Option<usize> = None;
                            for (i, entry) in state.shared_sublayers.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    if ui.button("Remove").clicked() {
                                        remove_at = Some(i);
                                    }
                                    ui.label(entry.as_str());
                                });
                            }
                            if let Some(i) = remove_at {
                                state.shared_sublayers.remove(i);
                            }
                            ui.add_space(4.0);
                            // Add row: pick a stage from the dropdown (or type a path)
                            // then Add. Blank and duplicate entries are rejected.
                            ui.horizontal(|ui| {
                                egui::ComboBox::from_id_salt("ps_shared_sublayer_pick")
                                    .selected_text("Pick stage…")
                                    .width(220.0)
                                    .show_ui(ui, |ui| {
                                        for opt in &state.stage_options {
                                            if ui
                                                .selectable_label(false, opt.as_str())
                                                .clicked()
                                            {
                                                state.new_shared_sublayer = opt.clone();
                                            }
                                        }
                                    });
                                ui.add(
                                    egui::TextEdit::singleline(&mut state.new_shared_sublayer)
                                        .hint_text("stages/_globals/prototypes.usda")
                                        .desired_width(240.0),
                                );
                                if ui.button("Add").clicked() {
                                    let candidate = state.new_shared_sublayer.trim().to_owned();
                                    if !candidate.is_empty()
                                        && !state.shared_sublayers.contains(&candidate)
                                    {
                                        state.shared_sublayers.push(candidate);
                                        state.new_shared_sublayer.clear();
                                    }
                                }
                            });
                        });

                    // ── Rendering ────────────────────────────────────────────
                    egui::CollapsingHeader::new("Rendering")
                        .default_open(true)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label("MSAA");
                                egui::ComboBox::from_id_salt("ps_msaa")
                                    .selected_text(state.msaa.to_string())
                                    .show_ui(ui, |ui| {
                                        for &n in MSAA_OPTIONS {
                                            ui.selectable_value(
                                                &mut state.msaa,
                                                n,
                                                n.to_string(),
                                            );
                                        }
                                    });
                            });
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                ui.label("Target FPS");
                                ui.add(
                                    egui::DragValue::new(&mut state.target_fps)
                                        .speed(1.0)
                                        .range(1..=1000),
                                );
                            });
                        });

                    // ── Cooking ──────────────────────────────────────────────
                    egui::CollapsingHeader::new("Cooking")
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.strong("Packaging");
                            ui.separator();
                            ui.checkbox(
                                &mut state.loose_levels,
                                "Keep levels as loose files (preserve payload composition)",
                            )
                            .on_hover_text(
                                "Stage referenced scenes as their original files instead of \
                                 baking everything into one stage.usdz. Loads much faster for \
                                 large scenes and preserves instancing exactly; the packaged \
                                 folder is larger. Leave off for small, shippable assets.",
                            );
                            ui.add_space(6.0);
                            ui.checkbox(
                                &mut state.embed_dev_token,
                                "⚠ Embed dev login token (TEMPORARY — do not distribute)",
                            )
                            .on_hover_text(
                                "Bakes your current logged-in bearer token into the cooked \
                                 run.toml so a Quest / Vision Pro build can fetch remote USD \
                                 assets (devices have no keychain). The build then CARRIES YOUR \
                                 CREDENTIALS — use only for your own device testing, never \
                                 distribute it. List the asset host under [network] allowed_hosts. \
                                 Temporary until the interactive logon-level flow exists.",
                            );
                            ui.add_space(10.0);

                            ui.strong("Android");
                            ui.separator();
                            egui::Grid::new("ps_android_grid")
                                .num_columns(2)
                                .spacing([10.0, 6.0])
                                .show(ui, |ui| {
                                    ui.label("JDK");
                                    path_picker_row(
                                        ui,
                                        &mut state.android_jdk,
                                        PathField::Jdk,
                                        &mut pick,
                                    );
                                    ui.end_row();
                                    ui.label("Android SDK");
                                    path_picker_row(
                                        ui,
                                        &mut state.android_sdk,
                                        PathField::Sdk,
                                        &mut pick,
                                    );
                                    ui.end_row();
                                    ui.label("Android NDK");
                                    path_picker_row(
                                        ui,
                                        &mut state.android_ndk,
                                        PathField::Ndk,
                                        &mut pick,
                                    );
                                    ui.end_row();
                                    ui.label("Texture quality")
                                        .on_hover_text(
                                            "How material/controller textures are capped on the \
                                             cooked Quest build. Full resolution keeps 4K sources \
                                             (~85 MiB each) and can exceed the device's per-frame \
                                             texture-upload budget — the second of two large \
                                             textures then falls back to a white/untextured \
                                             surface. The capped profiles keep every texture \
                                             resident.",
                                        );
                                    texture_quality_combo(
                                        ui,
                                        "ps_android_texquality",
                                        &mut state.android_texture_quality,
                                    );
                                    ui.end_row();
                                });

                            ui.add_space(10.0);
                            ui.strong("VisionOS");
                            ui.separator();
                            egui::Grid::new("ps_visionos_grid")
                                .num_columns(2)
                                .spacing([10.0, 6.0])
                                .show(ui, |ui| {
                                    ui.label("Identity");
                                    ui.add(
                                        egui::TextEdit::singleline(
                                            &mut state.visionos_identity,
                                        )
                                        .hint_text("Apple Development: Your Name (TEAMID)")
                                        .desired_width(300.0),
                                    );
                                    ui.end_row();
                                    ui.label("Provisioning profile");
                                    path_picker_row(
                                        ui,
                                        &mut state.visionos_provisioning_profile,
                                        PathField::ProvisioningProfile,
                                        &mut pick,
                                    );
                                    ui.end_row();
                                    ui.label("Bundle ID");
                                    ui.add(
                                        egui::TextEdit::singleline(
                                            &mut state.visionos_bundle_id,
                                        )
                                        .hint_text("com.example.project")
                                        .desired_width(300.0),
                                    );
                                    ui.end_row();
                                    ui.label("Team ID");
                                    ui.add(
                                        egui::TextEdit::singleline(
                                            &mut state.visionos_team_id,
                                        )
                                        .hint_text("TEAMID")
                                        .desired_width(300.0),
                                    );
                                    ui.end_row();
                                    ui.label("Texture quality")
                                        .on_hover_text(
                                            "How material/controller textures are capped on the \
                                             cooked Vision Pro build. Full resolution keeps 4K \
                                             sources (~85 MiB each) and can exceed the device's \
                                             per-frame texture-upload budget — large textures then \
                                             fall back to a white/untextured surface. The capped \
                                             profiles keep every texture resident.",
                                        );
                                    texture_quality_combo(
                                        ui,
                                        "ps_visionos_texquality",
                                        &mut state.visionos_texture_quality,
                                    );
                                    ui.end_row();
                                });
                        });

                    // ── Player ───────────────────────────────────────────────
                    egui::CollapsingHeader::new("Player")
                        .default_open(true)
                        .show(ui, |ui| {
                            ui.label("Player stage");
                            optional_path_combo(
                                ui,
                                "ps_player_stage",
                                &mut state.player_stage,
                                &state.stage_options,
                            );
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(
                                    "(none) uses the built-in player (desktop + XR). \
                                     Create one with File ▸ Create Player Stage to customize.",
                                )
                                .weak()
                                .italics(),
                            );
                        });

                    // ── Network ──────────────────────────────────────────────
                    egui::CollapsingHeader::new("Network")
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.checkbox(
                                &mut state.network_allow,
                                "Allow runtime network access",
                            )
                            .on_hover_text(
                                "Enable runtime USD assets, REST datasources and behavior HTTP. \
                                 Access requires this switch AND a matching allowed host.",
                            );
                            ui.add_space(6.0);
                            ui.label("Allowed hosts (one per line)");
                            ui.label(
                                egui::RichText::new(
                                     "Exact host (api.example.com) or *.domain wildcard. \
                                      Private addresses require a separate grant below; \
                                      link-local and cloud-metadata addresses are always blocked.",
                                )
                                .weak()
                                .italics(),
                            );
                            ui.add_space(4.0);
                            ui.add(
                                egui::TextEdit::multiline(&mut state.network_allowed_hosts)
                                    .hint_text("api.example.com\n*.data.example.org")
                                    .desired_width(360.0)
                                    .desired_rows(4),
                            );
                            ui.add_space(8.0);
                            ui.label("Private-network hosts (one per line)");
                            ui.add(egui::TextEdit::multiline(&mut state.network_private_hosts)
                                .hint_text("assets.internal.example\n127.0.0.1")
                                .desired_width(360.0).desired_rows(3));
                            ui.label(egui::RichText::new("Permit LAN/loopback for these hosts, including editor assets. Runtime hosts must also be allowlisted.").weak());
                            ui.add_space(8.0);
                            ui.label("Credential origins (one per line)");
                            ui.add(egui::TextEdit::multiline(&mut state.network_auth_origins)
                                .hint_text("https://api.example.com\nhttps://*.assets.example.org:8443")
                                .desired_width(360.0).desired_rows(3));
                            ui.label(egui::RichText::new("Explicit token-sharing destinations; independent of allowed hosts.").weak());
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                ui.label("Max response size (MB)");
                                ui.add(
                                    egui::DragValue::new(&mut state.network_max_response_mb)
                                        .speed(1)
                                        .range(0..=1024),
                                )
                                .on_hover_text(
                                    "Drop any REST response body larger than this. \
                                     0 uses the built-in default (1 MiB).",
                                );
                            });
                            ui.horizontal(|ui| {
                                ui.label("Max asset size (MiB)");
                                ui.add(egui::DragValue::new(&mut state.network_max_asset_mb).range(0..=u32::MAX));
                                ui.label(egui::RichText::new("0 = 256 MiB").weak());
                            });
                        });

                    // ── Behavior ─────────────────────────────────────────────
                    egui::CollapsingHeader::new("Behavior")
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(
                                    "Per-instance work-budget caps for behavior modules. \
                                     0 uses the runtime default. Raise the queue budget \
                                     (or lower the per-response reservation) to let a \
                                     behavior keep more than one async HTTP response in \
                                     flight at once.",
                                )
                                .weak()
                                .italics(),
                            );
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                ui.label("Queue budget (bytes)");
                                ui.add(
                                    egui::DragValue::new(&mut state.behavior_queue_bytes)
                                        .speed(4096.0)
                                        .range(0..=usize::MAX),
                                )
                                .on_hover_text(
                                    "Per-instance deferred-work byte budget (session edits, \
                                     overlays, async HTTP reservations). 0 uses the \
                                     built-in default (2 MiB).",
                                );
                                ui.label(egui::RichText::new("0 = 2 MiB").weak());
                            });
                            ui.horizontal(|ui| {
                                ui.label("HTTP response reservation (bytes)");
                                ui.add(
                                    egui::DragValue::new(&mut state.behavior_http_response_bytes)
                                        .speed(1024.0)
                                        .range(0..=usize::MAX),
                                )
                                .on_hover_text(
                                    "Cap and up-front reservation for a single async HTTP \
                                     response body. 0 uses the built-in default (1 MiB). \
                                     Also clamped by the network Max response size above.",
                                );
                                ui.label(egui::RichText::new("0 = 1 MiB").weak());
                            });
                            ui.horizontal(|ui| {
                                ui.label("Max in-flight HTTP requests");
                                ui.add(
                                    egui::DragValue::new(&mut state.behavior_http_inflight)
                                        .speed(1)
                                        .range(0..=1024),
                                )
                                .on_hover_text(
                                    "Max concurrent async HTTP requests per behavior \
                                     instance. 0 uses the built-in default (8).",
                                );
                                ui.label(egui::RichText::new("0 = 8").weak());
                            });
                        });

                    // ── Runtime prototypes ───────────────────────────────────
                    egui::CollapsingHeader::new("Runtime prototypes")
                        .default_open(false)
                        .show(ui, |ui| {
                            runtime_prototypes_section(
                                ui,
                                &mut state.runtime_prototypes,
                                &state.asset_options,
                            );
                        });

                    // ── Validation feedback ──────────────────────────────────
                    if !state.validation_errors.is_empty() {
                        ui.add_space(8.0);
                        for err in &state.validation_errors {
                            ui.colored_label(crate::theme::current().status_err, err);
                        }
                    }
                });

            ui.add_space(10.0);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button(egui::RichText::new("Save").strong()).clicked() {
                    let missing = state.missing_mandatory();
                    if missing.is_empty() {
                        state.validation_errors.clear();
                        action = Some(ProjectSettingsAction::Save);
                    } else {
                        state.validation_errors = missing;
                    }
                }
                if ui.button("Cancel").clicked() {
                    action = Some(ProjectSettingsAction::Cancel);
                }
            });
        });

    // A pick request takes precedence this frame (it keeps the dialog open).
    if let Some(field) = pick {
        return Some(ProjectSettingsAction::PickPath { field });
    }
    // A level interaction also keeps the dialog open; it outranks Save/Cancel so
    // a stray button state can't dismiss the dialog on the same frame.
    if level_action.is_some() {
        return level_action;
    }
    if !open && action.is_none() {
        action = Some(ProjectSettingsAction::Cancel);
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_state() -> ProjectSettingsState {
        ProjectSettingsState {
            name: "Demo".to_owned(),
            msaa: 4,
            target_fps: 90,
            ..ProjectSettingsState::default()
        }
    }

    #[test]
    fn name_is_mandatory() {
        let mut s = base_state();
        assert!(s.missing_mandatory().is_empty());
        s.name = "   ".to_owned();
        assert_eq!(s.missing_mandatory().len(), 1);
    }

    #[test]
    fn player_stage_is_optional() {
        // No player stage set → built-in player; Save may still proceed.
        let mut s = base_state();
        assert!(s.player_stage.is_none());
        assert!(s.missing_mandatory().is_empty());
        // Setting one keeps it valid.
        s.player_stage = Some("stages/player.usda".to_owned());
        assert!(s.missing_mandatory().is_empty());
    }

    #[test]
    fn transition_levels_are_optional() {
        // Start/transfer levels default to unset and never block Save.
        let mut s = base_state();
        assert!(s.start_level.is_none());
        assert!(s.transfer_level.is_none());
        assert!(s.missing_mandatory().is_empty());
        s.start_level = Some("stages/forest.usda".to_owned());
        s.transfer_level = Some("stages/_transfer.usda".to_owned());
        assert!(s.missing_mandatory().is_empty());
    }

    #[test]
    fn shared_sublayers_add_and_remove() {
        let mut s = base_state();
        assert!(s.shared_sublayers.is_empty());
        // Add two distinct entries, preserving order.
        s.shared_sublayers.push("stages/_globals/a.usda".to_owned());
        s.shared_sublayers.push("stages/_globals/b.usda".to_owned());
        assert_eq!(
            s.shared_sublayers,
            vec![
                "stages/_globals/a.usda".to_owned(),
                "stages/_globals/b.usda".to_owned(),
            ]
        );
        // Remove the first; the second survives and shifts down.
        s.shared_sublayers.remove(0);
        assert_eq!(s.shared_sublayers, vec!["stages/_globals/b.usda".to_owned()]);
    }
}
