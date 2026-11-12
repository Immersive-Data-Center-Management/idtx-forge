// Remote file browser + general app configuration panel.

// ── Remote file browser (feature = "remote" only) ────────────────────────────

#[cfg(feature = "remote")]
use idtx_remote::RemoteEntry;

/// UI state for the "Open Remote…" floating window.
#[cfg(feature = "remote")]
#[derive(Debug, Default, Clone)]
pub struct RemoteBrowserState {
    /// Full flat listing received from the server. `None` while loading or
    /// before the window is opened for the first time.
    pub entries: Option<Vec<RemoteEntry>>,
    /// Navigation stack — each entry is a directory name relative to the root.
    pub dir_stack: Vec<String>,
    /// True while the background listing is in-flight.
    pub loading: bool,
    /// Non-empty when the last listing attempt failed.
    pub error: Option<String>,
    /// Index into the *visible* rows (subdirs + files) of the selected row.
    pub selected: Option<usize>,
}

#[cfg(feature = "remote")]
impl RemoteBrowserState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Current path as a display string, e.g. `"/ scenes / city"`.
    #[must_use]
    pub fn breadcrumb(&self) -> String {
        if self.dir_stack.is_empty() {
            "/".to_owned()
        } else {
            format!("/ {}", self.dir_stack.join(" / "))
        }
    }

    /// Entries visible at the current navigation level.
    /// Subdirectories (sorted by name) come first, then USD files (sorted by name).
    #[must_use]
    pub fn visible_entries(&self) -> Vec<BrowserRow> {
        let Some(all) = self.entries.as_deref() else {
            return Vec::new();
        };
        let current_dir = if self.dir_stack.is_empty() {
            String::new()
        } else {
            self.dir_stack.join("/")
        };

        // Collect immediate subdirectory names at this level.
        let mut subdirs: Vec<String> = {
            let mut seen = std::collections::BTreeSet::new();
            for e in all {
                let dir = e.directory.trim_matches('/');
                let rest = if current_dir.is_empty() {
                    dir
                } else if dir == current_dir {
                    continue;
                } else if let Some(r) = dir.strip_prefix(&current_dir) {
                    match r.strip_prefix('/') {
                        Some(tail) => tail,
                        None => continue,
                    }
                } else {
                    continue;
                };
                if let Some(first) = rest.split('/').find(|c| !c.is_empty()) {
                    seen.insert(first.to_owned());
                }
            }
            seen.into_iter().collect()
        };
        subdirs.sort();

        let mut files: Vec<&RemoteEntry> = all
            .iter()
            .filter(|e| e.directory.trim_matches('/') == current_dir.as_str())
            .collect();
        files.sort_by(|a, b| a.filename.cmp(&b.filename));

        let mut rows: Vec<BrowserRow> = subdirs.into_iter().map(BrowserRow::Directory).collect();
        rows.extend(files.into_iter().map(|e| BrowserRow::File(e.clone())));
        rows
    }
}

/// A single row shown in the browser panel.
#[cfg(feature = "remote")]
#[derive(Debug, Clone)]
pub enum BrowserRow {
    Directory(String),
    File(RemoteEntry),
}

/// Intent emitted by [`render_remote_browser`] after a user interaction.
#[cfg(feature = "remote")]
#[derive(Debug)]
pub enum RemoteBrowserAction {
    /// User double-clicked a USD file — open it.
    Open(String),
    /// User closed the window.
    Close,
    /// User requested a (re-)listing of files.
    RequestListing,
}

// ── Session browser (feature = "remote") ─────────────────────────────────────

/// A single session entry received from `GET /api/v1/sessions`.
#[cfg(feature = "remote")]
#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub session_id: String,
    pub usd_file: String,
    pub mode: String,
    pub client_count: i64,
    pub created_at: i64,
    pub ws_url: String,
}

/// UI state for the "Connect Remote with Sync…" floating window.
#[cfg(feature = "remote")]
#[derive(Debug, Default, Clone)]
pub struct SessionBrowserState {
    /// Sessions received from the server. `None` while loading or before the
    /// first listing is fetched.
    pub entries: Option<Vec<SessionEntry>>,
    /// `true` while the background listing is in-flight.
    pub loading: bool,
    /// Non-empty when the last listing attempt failed.
    pub error: Option<String>,
    /// Index into `entries` of the currently highlighted row.
    pub selected: Option<usize>,
}

#[cfg(feature = "remote")]
impl SessionBrowserState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// Intent emitted by [`render_session_browser`] after a user interaction.
#[cfg(feature = "remote")]
#[derive(Debug)]
pub enum SessionBrowserAction {
    /// User selected a session and clicked "Connect" (or double-clicked).
    Join(SessionEntry),
    /// User closed the window.
    Close,
    /// User requested a (re-)listing of sessions.
    RequestListing,
}

/// Render the "Connect Remote with Sync…" floating window.
///
/// Returns `Some(action)` when the user triggered a meaningful intent,
/// `None` when the window is still open and idle.
#[cfg(feature = "remote")]
pub fn render_session_browser(
    ctx: &egui::Context,
    state: &mut SessionBrowserState,
    server_url: &str,
) -> Option<SessionBrowserAction> {
    let mut action: Option<SessionBrowserAction> = None;
    let mut open = true;

    egui::Window::new("Connect Remote with Sync…")
        .open(&mut open)
        .default_size([580.0, 420.0])
        .min_size([340.0, 240.0])
        .resizable(true)
        .show(ctx, |ui| {
            // ── Toolbar ──────────────────────────────────────────────────────
            ui.horizontal(|ui| {
                ui.strong("Active Collaboration Sessions");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("↻ Refresh").clicked() {
                        action = Some(SessionBrowserAction::RequestListing);
                    }
                });
            });
            ui.separator();

            // ── Server URL label ─────────────────────────────────────────────
            ui.label(
                egui::RichText::new(server_url)
                    .small()
                    .color(crate::theme::current().text_dim),
            );
            ui.add_space(4.0);

            // ── Loading / error states ───────────────────────────────────────
            if state.loading {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new());
                    ui.label("Loading sessions…");
                });
                return;
            }
            if let Some(ref err) = state.error.clone() {
                ui.colored_label(crate::theme::current().status_err, format!("Error: {err}"));
                if ui.button("Retry").clicked() {
                    action = Some(SessionBrowserAction::RequestListing);
                }
                return;
            }
            let Some(entries) = state.entries.as_ref() else {
                ui.label(
                    egui::RichText::new("No sessions loaded yet.").color(crate::theme::current().text_dim),
                );
                if ui.button("Load sessions").clicked() {
                    action = Some(SessionBrowserAction::RequestListing);
                }
                return;
            };

            // ── Session list ─────────────────────────────────────────────────
            if entries.is_empty() {
                ui.colored_label(
                    crate::theme::current().text_dim,
                    "(no active sessions on this server)",
                );
            } else {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (idx, entry) in entries.iter().enumerate() {
                        let is_selected = state.selected == Some(idx);
                        let filename = entry
                            .usd_file
                            .rsplit('/')
                            .next()
                            .unwrap_or(entry.usd_file.as_str());
                        let mode_label = match entry.mode.as_str() {
                            "collaborative_edit" => "collab-edit",
                            "collaborative_runtime" => "collab-runtime",
                            "single_edit" => "single-edit",
                            "single_runtime" => "single-runtime",
                            other => other,
                        };
                        let peers = if entry.client_count == 1 {
                            "1 peer".to_owned()
                        } else {
                            format!("{} peers", entry.client_count)
                        };
                        let row_text = format!("  🔗  {filename}  [{mode_label}]  —  {peers}");
                        let resp = ui.selectable_label(is_selected, &row_text);
                        if resp.clicked() {
                            state.selected = Some(idx);
                        }
                        if resp.double_clicked() {
                            action = Some(SessionBrowserAction::Join(entry.clone()));
                        }
                        // Show full path on hover.
                        resp.on_hover_text(&entry.usd_file);
                    }
                });
            }

            // ── Bottom bar ───────────────────────────────────────────────────
            ui.separator();
            ui.horizontal(|ui| {
                let can_join = state
                    .selected
                    .is_some_and(|sel| state.entries.as_ref().is_some_and(|e| sel < e.len()));
                ui.add_enabled_ui(can_join, |ui| {
                    if ui.button(egui::RichText::new("Connect").strong()).clicked() {
                        if let (Some(sel), Some(entries)) = (state.selected, state.entries.as_ref())
                        {
                            if let Some(entry) = entries.get(sel) {
                                action = Some(SessionBrowserAction::Join(entry.clone()));
                            }
                        }
                    }
                });
                if ui.button("Cancel").clicked() {
                    action = Some(SessionBrowserAction::Close);
                }
            });
        });

    if !open {
        action = Some(SessionBrowserAction::Close);
    }
    action
}

/// Render the "Open Remote…" floating window.
///
/// Returns `Some(action)` when the user triggered a meaningful intent,
/// `None` when the window is still open and idle.
///
/// `disabled_filepath`, when set, marks one file row (matched by `filepath`,
/// slash-normalized) as non-selectable with an explanatory hover — used when the
/// dialog doubles as the "Create Child → Asset…" picker to keep the user from
/// referencing the currently-open stage into itself.
#[cfg(feature = "remote")]
pub fn render_remote_browser(
    ctx: &egui::Context,
    state: &mut RemoteBrowserState,
    server_url: &str,
    disabled_filepath: Option<&str>,
) -> Option<RemoteBrowserAction> {
    let mut action: Option<RemoteBrowserAction> = None;
    let mut open = true;
    // Normalize once for comparison against each row's (also normalized) path.
    let disabled_norm = disabled_filepath.map(|p| p.replace('\\', "/"));

    egui::Window::new("Open Remote…")
        .open(&mut open)
        .default_size([520.0, 420.0])
        .min_size([320.0, 240.0])
        .resizable(true)
        .show(ctx, |ui| {
            // ── Toolbar ──────────────────────────────────────────────────────
            ui.horizontal(|ui| {
                ui.add_enabled_ui(!state.dir_stack.is_empty(), |ui| {
                    if ui.button("◀ Back").clicked() {
                        state.dir_stack.pop();
                        state.selected = None;
                    }
                });
                ui.separator();
                ui.monospace(state.breadcrumb());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("↻ Refresh").clicked() {
                        action = Some(RemoteBrowserAction::RequestListing);
                    }
                });
            });
            ui.separator();

            // ── Server URL label ─────────────────────────────────────────────
            ui.label(
                egui::RichText::new(server_url)
                    .small()
                    .color(crate::theme::current().text_dim),
            );
            ui.add_space(4.0);

            // ── Loading / error states ───────────────────────────────────────
            if state.loading {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new());
                    ui.label("Loading…");
                });
                return;
            }
            if let Some(ref err) = state.error.clone() {
                ui.colored_label(crate::theme::current().status_err, format!("Error: {err}"));
                if ui.button("Retry").clicked() {
                    action = Some(RemoteBrowserAction::RequestListing);
                }
                return;
            }
            if state.entries.is_none() {
                ui.label(
                    egui::RichText::new("No listing loaded yet.").color(crate::theme::current().text_dim),
                );
                if ui.button("Load listing").clicked() {
                    action = Some(RemoteBrowserAction::RequestListing);
                }
                return;
            }

            // ── File list ────────────────────────────────────────────────────
            let rows = state.visible_entries();
            if rows.is_empty() {
                ui.colored_label(crate::theme::current().text_dim, "(empty directory)");
            } else {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (idx, row) in rows.iter().enumerate() {
                        let is_selected = state.selected == Some(idx);
                        let (icon, label, size_str) = match row {
                            BrowserRow::Directory(name) => ("📁", name.as_str(), "—".to_owned()),
                            BrowserRow::File(e) => {
                                let size = format_size(e.size);
                                ("📄", e.filename.as_str(), size)
                            }
                        };
                        // A file row matching `disabled_filepath` is shown but not
                        // selectable (the current stage can't reference itself).
                        let row_disabled = match row {
                            BrowserRow::File(e) => disabled_norm
                                .as_deref()
                                .is_some_and(|d| e.filepath.replace('\\', "/") == d),
                            BrowserRow::Directory(_) => false,
                        };
                        let row_label = format!("  {icon}  {label}");
                        let resp = ui
                            .add_enabled_ui(!row_disabled, |ui| {
                                ui.selectable_label(is_selected, &row_label)
                            })
                            .inner;
                        let resp = if row_disabled {
                            resp.on_hover_text("This is the current stage.")
                        } else {
                            resp
                        };
                        ui.horizontal(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        egui::RichText::new(&size_str)
                                            .small()
                                            .color(crate::theme::current().text_dim),
                                    );
                                },
                            );
                        });
                        if row_disabled {
                            continue;
                        }
                        if resp.clicked() {
                            state.selected = Some(idx);
                        }
                        if resp.double_clicked() {
                            match row {
                                BrowserRow::Directory(name) => {
                                    state.dir_stack.push(name.clone());
                                    state.selected = None;
                                }
                                BrowserRow::File(e) => {
                                    action = Some(RemoteBrowserAction::Open(e.filepath.clone()));
                                }
                            }
                        }
                    }
                });
            }

            // ── Bottom bar ───────────────────────────────────────────────────
            ui.separator();
            ui.horizontal(|ui| {
                let can_open = state.selected.is_some_and(|sel| {
                    matches!(state.visible_entries().get(sel), Some(BrowserRow::File(_)))
                });
                ui.add_enabled_ui(can_open, |ui| {
                    if ui.button("Open").clicked() {
                        if let Some(sel) = state.selected {
                            if let Some(BrowserRow::File(e)) = state.visible_entries().get(sel) {
                                action = Some(RemoteBrowserAction::Open(e.filepath.clone()));
                            }
                        }
                    }
                });
                if ui.button("Cancel").clicked() {
                    action = Some(RemoteBrowserAction::Close);
                }
            });
        });

    if !open {
        action = Some(RemoteBrowserAction::Close);
    }
    action
}

// ── App configuration screen ──────────────────────────────────────────────────

/// Mutable state for the "Configure…" general settings modal.
#[derive(Debug, Clone, Default)]
pub struct AppConfigState {
    /// Server URL field (only when compiled with the `remote` feature).
    #[cfg(feature = "remote")]
    pub url: String,
    /// Server username field (only when compiled with the `remote` feature).
    #[cfg(feature = "remote")]
    pub username: String,
    /// Server password field (only when compiled with the `remote` feature).
    #[cfg(feature = "remote")]
    pub password: String,
    /// Status line shown below the server form (e.g. "Connected ✓" or an error).
    #[cfg(feature = "remote")]
    pub status: Option<String>,
    /// `true` when `status` represents a success message.
    #[cfg(feature = "remote")]
    pub status_ok: bool,
}

/// Intent emitted by [`app_config_window`].
#[derive(Debug)]
pub enum AppConfigAction {
    /// User clicked "Save" for the server configuration section.
    #[cfg(feature = "remote")]
    Save {
        url: String,
        username: String,
        password: String,
    },
    /// User clicked "Test Connection".
    #[cfg(feature = "remote")]
    Test {
        url: String,
        username: String,
        password: String,
    },
    /// User dismissed the dialog.
    Close,
}

/// Render the general "Configuration" modal.
///
/// The window always appears when called. Sections that require the `remote`
/// feature are compiled in only when that feature is active; without it the
/// window displays a placeholder until additional sections are added.
pub fn app_config_window(
    ctx: &egui::Context,
    #[cfg_attr(not(feature = "remote"), allow(unused_variables))] state: &mut AppConfigState,
) -> Option<AppConfigAction> {
    let mut action: Option<AppConfigAction> = None;
    let mut open = true;

    egui::Window::new("Configuration")
        .open(&mut open)
        .default_size([440.0, 280.0])
        .resizable(false)
        .show(ctx, |ui| {
            #[cfg(feature = "remote")]
            {
                ui.add_space(4.0);
                ui.strong("Server");
                ui.separator();
                ui.add_space(4.0);

                egui::Grid::new("server_config_grid")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Server URL:");
                        ui.add(
                            egui::TextEdit::singleline(&mut state.url)
                                .hint_text("https://idtx.example.com")
                                .desired_width(280.0),
                        );
                        ui.end_row();

                        ui.label("Username:");
                        ui.add(
                            egui::TextEdit::singleline(&mut state.username)
                                .hint_text("alice")
                                .desired_width(280.0),
                        );
                        ui.end_row();

                        ui.label("Password:");
                        ui.add(
                            egui::TextEdit::singleline(&mut state.password)
                                .password(true)
                                .desired_width(280.0),
                        );
                        ui.end_row();
                    });

                if let Some(ref status) = state.status {
                    ui.add_space(6.0);
                    let color = if state.status_ok {
                        crate::theme::current().status_ok
                    } else {
                        crate::theme::current().status_err
                    };
                    ui.colored_label(color, status);
                }

                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    let any_empty = state.url.is_empty() || state.username.is_empty();
                    ui.add_enabled_ui(!any_empty, |ui| {
                        if ui.button("Test Connection").clicked() {
                            action = Some(AppConfigAction::Test {
                                url: state.url.clone(),
                                username: state.username.clone(),
                                password: state.password.clone(),
                            });
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Cancel").clicked() {
                            action = Some(AppConfigAction::Close);
                        }
                        ui.add_enabled_ui(!any_empty, |ui| {
                            if ui.button(egui::RichText::new("Save").strong()).clicked() {
                                action = Some(AppConfigAction::Save {
                                    url: state.url.clone(),
                                    username: state.username.clone(),
                                    password: state.password.clone(),
                                });
                            }
                        });
                    });
                });
            }

            #[cfg(not(feature = "remote"))]
            {
                ui.add_space(8.0);
                ui.colored_label(
                    crate::theme::current().text_dim,
                    "No configuration options available.",
                );
                ui.add_space(12.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        action = Some(AppConfigAction::Close);
                    }
                });
            }
        });

    if !open {
        action = Some(AppConfigAction::Close);
    }
    action
}

// ── helpers ───────────────────────────────────────────────────────────────────

#[cfg(feature = "remote")]
fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    // Integer-only formatting: `whole.tenths UNIT` to avoid float casts
    // (the workspace forbids `cast_precision_loss`).
    if bytes >= GB {
        let whole = bytes / GB;
        let tenths = (bytes % GB) / (GB / 10);
        format!("{whole}.{tenths} GB")
    } else if bytes >= MB {
        let whole = bytes / MB;
        let tenths = (bytes % MB) / (MB / 10);
        format!("{whole}.{tenths} MB")
    } else if bytes >= KB {
        let whole = bytes / KB;
        let tenths = (bytes % KB) / (KB / 10);
        format!("{whole}.{tenths} KB")
    } else {
        format!("{bytes} B")
    }
}
