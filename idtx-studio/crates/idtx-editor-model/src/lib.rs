//! UI-neutral product profiles, editor command contracts, and inspector data
//! transfer objects for IDTX Studio.

pub mod level_transition;

pub use level_transition::{LevelPath, LevelTransition, TransitionPhase, TransitionStep};

/// UI-neutral editor command vocabulary.
///
/// Pure egui panels (in `idtx-editor-ui`) emit these intents; the shell
/// (`idtx-studio-shell`) interprets them against the live session. Only
/// commands with a cross-panel or cross-product contract belong here — panel
/// -local state changes stay inside the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorCommand {
    /// Launch the active stage in the out-of-process `idtx-runtime` player
    /// (`OpenXR` preview). Gated by the UI on an open stage and
    /// [`ProductProfile::xr_preview_enabled`]. The runtime process probes for
    /// an `OpenXR` device itself and falls back to a desktop preview when none is
    /// present, so the editor itself does not detect XR devices.
    LaunchRuntimePreview,
    /// Launch the active stage in the out-of-process `idtx-runtime` player in
    /// **desktop** (flat, no-HMD) mode — the same on-the-fly assembly-stage
    /// handoff as [`EditorCommand::LaunchRuntimePreview`], but presented in a
    /// plain window with the orbit camera and no `OpenXR` session. Lets
    /// authoring behaviors be previewed as a packaged experience without a
    /// headset.
    LaunchRuntimePreviewDesktop,
    /// Stop the running runtime-preview process for the active stage.
    StopRuntimePreview,
    /// Cook + package the active project into a launchable bundle for the given
    /// [`BuildTarget`]. Emitted by the Conductor "Build" menu; surfaced by the
    /// shell only when built with the `cooking` (⟹ `build-tools`) feature, and
    /// gated by the UI on an active project. The heavy cook runs off the UI
    /// thread — the shell reports progress and the final bundle path.
    BuildExperience(BuildTarget),
}

/// The distribution target the Conductor "Build" menu cooks for.
///
/// Deliberately coarse and UI-neutral: it names the *shape* of the experience
/// (flat desktop vs. PCVR), not a Rust target triple. The shell maps it to a
/// concrete `idtx_build::CookTarget` for the host it is running on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildTarget {
    /// A flat, no-HMD desktop player (`mode = "desktop"`).
    Desktop,
    /// A PC-tethered VR player (`mode = "xr"`, `pcvr` feature).
    Pcvr,
    /// A standalone Android/Quest headset player (`mode = "xr"`, `standalone` +
    /// `quest` features, `aarch64-linux-android`). Foreign-triple: the editor
    /// cooks the portable payload and defers packaging to `xtask`/CI.
    Quest,
    /// A standalone Apple Vision Pro player (`mode = "xr"`, `standalone` +
    /// `visionos` features, `aarch64-apple-visionos`). Foreign-triple and
    /// macOS-only to package; the editor cooks the payload only.
    VisionPro,
}

impl BuildTarget {
    /// Human-facing menu label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Desktop => "Desktop",
            Self::Pcvr => "PCVR",
            Self::Quest => "Quest",
            Self::VisionPro => "VisionPro",
        }
    }

    /// Whether this target runs an XR session (drives the cooked `mode` and the
    /// XR cook features).
    #[must_use]
    pub const fn is_xr(self) -> bool {
        matches!(self, Self::Pcvr | Self::Quest | Self::VisionPro)
    }

    /// Whether this target is a standalone (on-device, foreign-triple) headset
    /// player — `true` for [`Self::Quest`] and [`Self::VisionPro`]. Standalone
    /// targets build for a foreign Rust triple, so the editor cooks the portable
    /// payload and leaves packaging to `xtask`/CI.
    #[must_use]
    pub const fn is_standalone(self) -> bool {
        matches!(self, Self::Quest | Self::VisionPro)
    }
}

/// Identifies which IDTX Studio product binary is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductKind {
    /// Read-only USD inspector.
    Inspector,
    /// Transform and variant authoring.
    Poser,
    /// Project-level authoring and orchestration.
    Conductor,
}

/// Runtime capability profile that parameterises the shared shell.
///
/// The three app binaries each pass a different `ProductProfile` to
/// [`idtx_studio_shell::run`]:
///
/// - `idtx-inspector` → [`ProductProfile::INSPECTOR`]
/// - `idtx-poser`     → [`ProductProfile::POSER`]
/// - `idtx-conductor` → [`ProductProfile::CONDUCTOR`]
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy)]
pub struct ProductProfile {
    pub kind: ProductKind,
    /// Raw bytes of the window/taskbar icon (ICO or PNG), or `None` to
    /// fall back to the platform default. Each flavor binary supplies
    /// its own icon via `include_bytes!` in its `Capabilities`
    /// constructor (the constants below default to `None`; the binary
    /// crates override the field). Decoded at window-creation time with
    /// the `image` crate and applied through `Window::set_window_icon`,
    /// which drives the *title-bar / taskbar* icon at runtime (distinct
    /// from the icon embedded into the `.exe` resource table by the
    /// build script, which drives the Explorer file icon).
    pub window_icon: Option<&'static [u8]>,
    /// Whether this product manages **projects** — creating, opening, and
    /// discovering an `idtx-project` (`project.toml`) and its root-scoped asset
    /// browser. Only the Conductor; the single-stage Inspector/Poser open a
    /// stage directly and never carry a project.
    pub project_management: bool,
    /// Whether this product authors **levels** — the ordered level-stage
    /// registry that layers onto a project. Conductor only.
    pub level_authoring: bool,
    /// Whether this product authors **behaviors** — attaching and configuring
    /// sandboxed WASM behavior modules on prims. Conductor only.
    pub behavior_authoring: bool,
    /// Whether this product exposes **player transport** controls (play / stop
    /// the in-editor behavior simulation). Conductor only; the Inspector is
    /// read-only and the Poser edits statically.
    pub player_controls: bool,
    /// Whether this product renders soft shadows at all. The global
    /// "shadow-rendering-off" switch as a product capability: `false` for the
    /// read-only Inspector and the transform-focused Poser (shadows are a
    /// look-dev / authoring concern), `true` for the Conductor. Gates the
    /// stage-authored `idtx:softShadows` flag downstream — both must be true for
    /// a shadow pass to run. See `docs/shadow-rendering-plan.md`.
    pub shadows_enabled: bool,
}

impl ProductProfile {
    pub const INSPECTOR: Self = Self {
        kind: ProductKind::Inspector,
        window_icon: None,
        project_management: false,
        level_authoring: false,
        behavior_authoring: false,
        player_controls: false,
        shadows_enabled: false,
    };
    pub const POSER: Self = Self {
        kind: ProductKind::Poser,
        window_icon: None,
        project_management: false,
        level_authoring: false,
        behavior_authoring: false,
        player_controls: false,
        shadows_enabled: true,
    };
    pub const CONDUCTOR: Self = Self {
        kind: ProductKind::Conductor,
        window_icon: None,
        project_management: true,
        level_authoring: true,
        behavior_authoring: true,
        player_controls: true,
        shadows_enabled: true,
    };

    #[must_use]
    pub const fn kind(self) -> ProductKind {
        self.kind
    }

    /// Whether this product manages projects (create/open/discover + the
    /// project-rooted asset browser).
    #[must_use]
    pub const fn project_management(self) -> bool {
        self.project_management
    }

    /// Whether this product authors the project's level-stage registry.
    #[must_use]
    pub const fn level_authoring(self) -> bool {
        self.level_authoring
    }

    /// Whether this product authors WASM behavior modules on prims.
    #[must_use]
    pub const fn behavior_authoring(self) -> bool {
        self.behavior_authoring
    }

    /// Whether this product exposes in-editor player transport (play/stop).
    #[must_use]
    pub const fn player_controls(self) -> bool {
        self.player_controls
    }
    /// Whether this product renders soft shadows (the product-level half of the
    /// shadow enable decision; the stage `idtx:softShadows` flag is the other).
    #[must_use]
    pub const fn shadows_enabled(self) -> bool {
        self.shadows_enabled
    }

    /// Window title for this product.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self.kind {
            ProductKind::Inspector => "IDTX Inspector",
            ProductKind::Poser => "IDTX Poser",
            ProductKind::Conductor => "IDTX Conductor",
        }
    }

    /// Whether the asset browser panel should be shown in the shell layout.
    /// Only true for the Conductor product.
    #[must_use]
    pub const fn show_asset_browser(self) -> bool {
        matches!(self.kind, ProductKind::Conductor)
    }
}

// ── Inspector data transfer objects ──────────────────────────────────────────

/// Lightweight prim descriptor assembled by the shell from the ECS world.
///
/// Passed to the inspector panel functions instead of raw ECS access so that
/// `idtx-editor-ui` does not need to depend on `idtx-scene`.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default)]
pub struct PrimInfo {
    /// Absolute `SdfPath` string, e.g. `"/World/Geometry/Cube"`.
    pub path: String,
    /// USD schema type name, e.g. `"Mesh"`, `"Camera"`, `"UsdLuxDomeLight"`, or `""`.
    pub type_name: String,
    /// USD kind metadata: `"component"`, `"assembly"`, `"group"`, or `""`.
    pub kind: String,
    /// API schemas applied to this prim (e.g. `"SkelBindingAPI"`).
    pub applied_schemas: Vec<String>,
    pub is_visible: bool,
    /// True when this prim has a payload arc.
    pub has_payload: bool,
    /// True when the payload arc is currently loaded.
    pub payload_loaded: bool,
    /// True when this prim is a packed-stage reference boundary.
    pub is_packed_boundary: bool,
    /// True when this prim has root-layer opinions inside a packed boundary.
    pub is_locally_authored: bool,
    /// Variant sets on this prim (name → current selection + all choices).
    pub variant_sets: Vec<PrimVariantSet>,
    /// Local-to-parent transform matrix from `UsdGeomXformable::GetLocalTransformation()`.
    /// Row-major, row-vector convention (`p' = p · M`). `None` when the prim is not xformable.
    pub local_transform: Option<[[f32; 4]; 4]>,
    /// The prim's **local-space** axis-aligned bounds (`min`, `max`), or `None`
    /// when the prim has no hydrated bounds. Used by the collision inspector to
    /// seed mesh-derived default dimensions when a collision shape is chosen.
    pub local_bounds: Option<([f32; 3], [f32; 3])>,
    /// Classification and raw authored values of the prim's `xformOpOrder`.
    ///
    /// Populated by the shell from `idtx_usd::authored_xform_stack`. `None` when
    /// the prim is not xformable or the authoring feature is compiled out. When
    /// present the inspector prefers this over decomposing [`Self::local_transform`]
    /// so a rotation like `Y = 180°` round-trips exactly instead of folding onto
    /// the other axes (a matrix→Euler decomposition can never place a middle-axis
    /// rotation outside ±90°).
    pub authored_transform: Option<AuthoredTransform>,
    /// The network role governing behavior execution for this prim, resolved by
    /// walking to its nearest `PlayerRole` ancestor.
    ///
    /// A token — `"local"`, `"remote"`, `"server"`, or `"unowned"` (the same
    /// vocabulary as `idtx:playerRole`) — kept as a plain `String` so this crate
    /// stays free of an `idtx-scene` dependency. Populated by the shell from
    /// `StudioSession::resolved_player_role`. `None` when the prim has no
    /// hydrated entity or the role could not be resolved; `Some("local")` is the
    /// single-player default (no `PlayerRole` ancestor). The Behaviors panel uses
    /// this to warn that a behavior on a `remote` (or unopted `server`) pawn will
    /// not tick locally.
    pub resolved_player_role: Option<String>,
}

/// How a prim's `xformOpOrder` maps onto the inspector's translate/rotate/scale rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum XformEditKind {
    /// Canonical/partial translate + `rotateXYZ` + scale op stack (or an empty
    /// stack that authoring will bootstrap). The authored Euler values are exact,
    /// so the inspector reads and writes them directly with no matrix round-trip.
    #[default]
    TrsOps,
    /// A single `xformOp:transform` matrix. Decomposed for display and recomposed
    /// on write; the Euler middle-axis ambiguity is inherent to the matrix, so the
    /// inspector holds the working value during a drag and never restructures the
    /// op order.
    Matrix,
    /// Pivots, inverse ops, `orient`, non-`rotateXYZ` rotate orders, or a matrix
    /// mixed with other ops. Not safely editable → the transform rows are read-only.
    Unsupported,
}

/// The authored transform of a prim as classified for the inspector.
///
/// For [`XformEditKind::TrsOps`] the TRS fields carry the raw authored op values
/// (identity defaults for absent ops). For [`XformEditKind::Matrix`] and
/// [`XformEditKind::Unsupported`] the TRS fields are unspecified and the inspector
/// falls back to decomposing [`PrimInfo::local_transform`].
#[derive(Debug, Clone, PartialEq)]
pub struct AuthoredTransform {
    pub kind: XformEditKind,
    pub translation: [f64; 3],
    /// XYZ Euler rotation in degrees, read from the authored `rotateXYZ` op.
    pub rotation_xyz_degrees: [f64; 3],
    pub scale: [f64; 3],
    /// Human-readable reason the stack is not editable (`Unsupported` only).
    pub diagnostic: Option<String>,
}

/// One variant set as seen on a prim.
#[derive(Debug, Clone, Default)]
pub struct PrimVariantSet {
    pub name: String,
    pub selection: String,
    pub choices: Vec<String>,
    /// Strongest-first `"<layerId>:<selection>"` provenance of the selection
    /// opinion — which layers author a selection for this set.
    pub provenance: Vec<String>,
    /// `true` when this variant set is declared on an editable local layer
    /// (root/session/sublayer), so its contents can be authored into via a
    /// variant edit context.  `false` when it arrives only across a reference
    /// or payload arc — then only the selection or a local override is allowed.
    pub defined_on_local_layer: bool,
}

/// Which composition arc introduced a layer into the stage's layer stack.
///
/// UI-neutral mirror of `idtx_usd::LayerArcKind`, kept here so `idtx-editor-ui`
/// need not depend on `idtx-usd` for the Layer Stack panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LayerArc {
    /// The anonymous session layer (strongest).
    Session,
    /// The stage root layer.
    #[default]
    Root,
    /// A sublayer pulled in by a `subLayers` arc (recursively).
    SubLayer,
    /// A layer pulled in by a `references` arc on some prim.
    Reference,
    /// A layer pulled in by a `payload` arc on some prim (may be unloaded).
    Payload,
}

/// One layer in the stage's composed layer stack, as rendered by the Layer
/// Stack panel.  Assembled by the shell from `idtx_usd::LayerRecord`.
///
/// The panel renders these strongest-first; `depth` drives indentation and the
/// arc + flags drive the row's icon, badges, and styling.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default)]
pub struct LayerInfo {
    /// Full layer identifier (file path, URI, or `anon:0x…`).
    pub identifier: String,
    /// Human-readable display name (usually the file basename).
    pub display_name: String,
    /// Resolved real filesystem path; `None` for anonymous/session layers.
    pub real_path: Option<String>,
    /// The arc that introduced this layer.
    pub arc: LayerArc,
    /// For `Reference`/`Payload` arcs: the introducing prim path (e.g.
    /// `/World/Pawn`). `None` for local session/root/sublayer arcs.
    pub arc_source_path: Option<String>,
    /// For `Reference`/`Payload` arcs: the target prim path inside the
    /// referenced layer (e.g. `/Pawn`). `None` for local arcs.
    pub arc_target_path: Option<String>,
    /// Whether this arc's node contributes prim specs. Always `true` for local
    /// arcs.
    pub has_specs: bool,
    /// Whether the arc's content is loaded. `false` only for a deferred
    /// (unloaded) payload.
    pub is_loaded: bool,
    /// Nesting depth for indentation (session and root are `0`).
    pub depth: u32,
    /// `SdfLayerOffset` offset applied by the parent sublayer arc.
    pub offset: f64,
    /// `SdfLayerOffset` time scale applied by the parent sublayer arc.
    pub scale: f64,
    /// Number of prim specs this layer contributes (the `Δ` badge).
    pub delta_count: u32,
    pub is_anonymous: bool,
    pub is_dirty: bool,
    pub is_muted: bool,
    /// True when this layer is the stage's current edit target.
    pub is_edit_target: bool,
    /// Identifier of the layer that directly introduced this layer.
    /// Empty for session/root.  For sublayers: the containing layer.
    /// For reference/payload: the anchor layer that authors the arc spec.
    pub parent_identifier: String,
}

/// The full stage layer stack, strongest-first, as shown in the Layer Stack tab.
#[derive(Debug, Clone, Default)]
pub struct LayerStackInfo {
    pub layers: Vec<LayerInfo>,
}

/// Controls whether the inspector shows editing widgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InspectorMode {
    #[default]
    ReadOnly,
    /// Variant-set combos are interactive; all other widgets remain read-only.
    ReadOnlyWithVariants,
    /// Full attribute editing enabled (Poser product). Variant-set combos and
    /// all claimed-editable attributes show interactive widgets.
    Authoring,
    /// Full attribute editing enabled & Cooking available (Conductor Product)
    Cooking,
}

/// Typed attribute value for inspector authoring.
///
/// Mirrors `idtx_usd::PropertyValue` but lives in `idtx-editor-model` so that
/// `idtx-editor-ui` and the `EditSink` trait have no dependency on the
/// `authoring` feature of `idtx-usd`.
#[derive(Debug, Clone, PartialEq)]
pub enum AttributeValue {
    Bool(bool),
    F32(f32),
    F64(f64),
    String(String),
    Token(String),
    /// A `token[]` value (e.g. the `IdtxCollisionAPI` channel list).
    TokenArray(Vec<String>),
    /// Linear-light RGB, each component in [0, 1].
    Color3f([f32; 3]),
    Asset(String),
    /// A scalar `int` value (e.g. `idtx:behavior:limits:memoryMb`).
    Int(i32),
}

/// Independent actor preview; separate from the stage timeline transport.
#[derive(Debug, Clone, Copy)]
pub enum AnimationTransport {
    Play, Pause, Stop, Seek(f64), SetSpeed(f64), SetLooping(bool), Release,
}

/// Where an inspector attribute edit should be authored.
///
/// Mirrors `idtx_usd::EditTarget`.  Lives here so `idtx-editor-ui` and the
/// `EditSink` trait carry the target without depending on `idtx-usd`.  Edits
/// default to [`EditTarget::Root`] (a bare prim-local opinion in the stage root
/// layer — the historical behavior).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum EditTarget {
    /// Author into the stage root layer as a prim-local opinion (strongest;
    /// shadows any variant opinion for the same property).
    #[default]
    Root,
    /// Author into the anonymous session layer (transient, never saved, never
    /// synced to the collaboration backend).
    Session,
    /// Author into the named variant of a variant set, via the variant edit
    /// context, so the opinion is scoped to `{set_name=variant_name}`.
    Variant {
        set_name: String,
        variant_name: String,
    },
}

/// Decomposed transform edit from the inspector panel.
///
/// All three components are always present so the authoring layer can
/// recompose the full TRS into whatever xformOps the prim already has.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransformEdit {
    pub translation: [f64; 3],
    /// XYZ Euler rotation in degrees.
    pub rotation_xyz_degrees: [f64; 3],
    pub scale: [f64; 3],
}

/// Tunable parameters for collision convex-hull generation, surfaced as
/// drag-value rows in the collision inspector panel and forwarded through the
/// [`crate`]-neutral `EditSink` to the authoring layer (which maps them to
/// `idtx_hull_gen::HullParams`). Kept here so the pure-UI crate stays USD-free.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HullGenParams {
    /// Input-point budget; larger clouds are decimated to this before hulling.
    pub max_input_points: u32,
    /// Cap on the generated hull's vertex count — the "collision detail" knob.
    /// A high-vertex scanned hull makes a dynamic body jitter and never sleep;
    /// the generator reduces it to at most this many well-distributed vertices.
    /// Clamped to at least 4 (a tetrahedron) in the UI and generator.
    pub max_hull_vertices: u32,
}

impl Default for HullGenParams {
    fn default() -> Self {
        Self {
            max_input_points: 20_000,
            max_hull_vertices: 48,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BuildTarget, ProductKind, ProductProfile};

    #[test]
    fn build_target_labels_and_xr_flag() {
        assert_eq!(BuildTarget::Desktop.label(), "Desktop");
        assert_eq!(BuildTarget::Pcvr.label(), "PCVR");
        assert_eq!(BuildTarget::Quest.label(), "Quest");
        assert_eq!(BuildTarget::VisionPro.label(), "VisionPro");
        assert!(!BuildTarget::Desktop.is_xr());
        assert!(BuildTarget::Pcvr.is_xr());
        assert!(BuildTarget::Quest.is_xr());
        assert!(BuildTarget::VisionPro.is_xr());
    }

    #[test]
    fn only_standalone_targets_are_standalone() {
        assert!(!BuildTarget::Desktop.is_standalone());
        assert!(!BuildTarget::Pcvr.is_standalone());
        assert!(BuildTarget::Quest.is_standalone());
        assert!(BuildTarget::VisionPro.is_standalone());
        // Every standalone target is also an XR target.
        for t in [BuildTarget::Quest, BuildTarget::VisionPro] {
            assert!(t.is_standalone() && t.is_xr());
        }
    }

    #[test]
    fn inspector_is_read_only_across_all_capabilities() {
        let p = ProductProfile::INSPECTOR;
        assert_eq!(p.kind(), ProductKind::Inspector);
        assert!(!p.project_management());
        assert!(!p.level_authoring());
        assert!(!p.behavior_authoring());
        assert!(!p.player_controls());
        assert!(!p.show_asset_browser());
    }

    #[test]
    fn poser_authors_stages_but_not_project_level_features() {
        let p = ProductProfile::POSER;
        assert_eq!(p.kind(), ProductKind::Poser);
        assert!(!p.project_management());
        assert!(!p.level_authoring());
        assert!(!p.behavior_authoring());
        assert!(!p.player_controls());
        assert!(!p.show_asset_browser());
    }

    #[test]
    fn conductor_enables_every_capability() {
        let p = ProductProfile::CONDUCTOR;
        assert_eq!(p.kind(), ProductKind::Conductor);
        assert!(p.project_management());
        assert!(p.level_authoring());
        assert!(p.behavior_authoring());
        assert!(p.player_controls());
        assert!(p.show_asset_browser());
    }

    #[test]
    fn only_conductor_manages_projects_and_shows_asset_browser() {
        // The project-rooted asset browser and project management are the seam
        // the Conductor CLI/dialog work builds on; they must not leak into the
        // single-stage products.
        assert!(ProductProfile::CONDUCTOR.project_management());
        assert_eq!(
            ProductProfile::CONDUCTOR.show_asset_browser(),
            ProductProfile::CONDUCTOR.project_management()
        );
        assert!(!ProductProfile::INSPECTOR.project_management());
        assert!(!ProductProfile::POSER.project_management());
    }
}
