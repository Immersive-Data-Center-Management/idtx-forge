//! Codeless `OpenUSD` schema plugins for IDTX.
//!
//! An IDTX *USD extension* ships custom `IsA` / `HasAPI` schemas as an `OpenUSD`
//! plugin so that `idtx:`-namespaced content validates (`HasAPI`, `apiSchemas`,
//! `allowedTokens`).
//!
//! # `IdtxCollisionAPI` (codeless, single-apply / `HasAPI`)
//!
//! A **codeless** schema compiles no C++: it is pure resource data
//! (`plugInfo.json` + `generatedSchema.usda`) registered at runtime. Because the
//! schema registry builds from `generatedSchema.usda`, the existing bridge calls
//! ([`idtx_usd::StageService::prim_has_api`], [`idtx_usd::StageService::eval_attribute`],
//! and `StructuralCommand::ApplyApi`) recognise the schema with no `idtx-usd-sys`
//! changes.
//!
//! Collidability is authored with the `OpenUSD`-standard
//! [`PHYSICS_COLLISION_SCHEMA`] (`UsdPhysicsCollisionAPI`, +
//! [`PHYSICS_MESH_COLLISION_SCHEMA`] on meshes). `IdtxCollisionAPI` is a
//! **reduced addendum** applied alongside, carrying only what the standard
//! cannot persist:
//!
//! | attribute                      | type           | meaning |
//! |--------------------------------|----------------|---------|
//! | `idtx:collision:hull:points`   | `point3f[]`    | pre-baked convex-hull vertices (local space) |
//! | `idtx:collision:hull:triangles`| `int[]`        | flat triangle index list into `hull:points` |
//! | `idtx:collision:channel`       | `token[]`      | [`CollisionChannel`] list — `All`/`Physics`/`Interaction` |
//!
//! # Usage
//!
//! Call [`install`] once, **before** opening any stage that uses the schema, so
//! the type is in the registry when the stage is composed:
//!
//! ```no_run
//! idtx_usd_extension::install().expect("register IDTX USD schemas");
//! // ... now StageService::open(..) resolves IdtxCollisionAPI
//! ```

use std::path::PathBuf;

pub use idtx_usd::StageError;

// ── schema / attribute identifiers ──────────────────────────────────────────

/// USD type name of the collision `HasAPI` schema, as reported by `apiSchemas`
/// and accepted by [`idtx_usd::StageService::prim_has_api`].
pub const COLLISION_SCHEMA: &str = "IdtxCollisionAPI";

/// Property namespace shared by every `IdtxCollisionAPI` attribute.
pub const COLLISION_NS: &str = "idtx:collision";

/// USD type name of the `IdtxCollisionBodyAPI` single-apply schema — a
/// property-less marker applied to an `Xformable` grouping prim that declares
/// its subtree one IDTX collision / interaction body. A raycast hit on a
/// descendant collider resolves up to the nearest-ancestor body (see
/// [`idtx_scene::collision_body_of`] / `Scene::collision_body`), so gaze /
/// highlight / grab act on the whole asset. Carries no geometry; the static
/// analogue of a `UsdPhysicsRigidBodyAPI` grouping prim.
pub const COLLISION_BODY_SCHEMA: &str = "IdtxCollisionBodyAPI";

// ── OpenUSD-standard physics collision schemas (usdPhysics plugin) ───────────
//
// A prim becomes collidable by applying the standard `UsdPhysicsCollisionAPI`
// (+ `UsdPhysicsMeshCollisionAPI` on meshes). The reduced [`COLLISION_SCHEMA`]
// below is an *addendum* applied alongside, carrying only data the standard
// cannot persist (pre-baked hull + IDTX channel). usdPhysics ships in the
// linked OpenUSD monolith, so `prim_has_api` / `eval_attribute` resolve these
// names with no bridge changes.

/// USD type name of the `OpenUSD`-standard `UsdPhysicsScene` concrete `IsA` prim
/// (`PrimNode.type_name`). Its presence in the hydrated stage is the sole gate
/// that activates the runtime physics simulation (plan §3) — no IDTX custom
/// opt-in. Resolved from the linked `usdPhysics` plugin, so
/// [`idtx_usd::StageService`] reports it with no bridge changes.
pub const PHYSICS_SCENE_TYPE: &str = "PhysicsScene";

/// `vector3f` — `physics:gravityDirection` on `UsdPhysicsScene`. A zero vector
/// is the "unset" sentinel ⇒ gravity falls along `-upAxis` (plan §4).
pub const ATTR_PHYSICS_GRAVITY_DIRECTION: &str = "physics:gravityDirection";
/// `float` — `physics:gravityMagnitude` on `UsdPhysicsScene`. A negative value
/// is the "unset" sentinel ⇒ earth gravity scaled by `metersPerUnit` (plan §4).
pub const ATTR_PHYSICS_GRAVITY_MAGNITUDE: &str = "physics:gravityMagnitude";

// ── OpenUSD-standard rigid-body / mass / material schemas (usdPhysics) ────────

/// USD type name of the `OpenUSD`-standard `UsdPhysicsRigidBodyAPI` single-apply
/// schema — upgrades a collider (or whole-asset body) to a simulated rigid body.
pub const PHYSICS_RIGID_BODY_SCHEMA: &str = "PhysicsRigidBodyAPI";
/// USD type name of the `OpenUSD`-standard `UsdPhysicsMassAPI` single-apply schema
/// (mass / density / centre-of-mass overrides).
pub const PHYSICS_MASS_SCHEMA: &str = "PhysicsMassAPI";
/// USD type name of the `OpenUSD`-standard `UsdPhysicsMaterialAPI` single-apply
/// schema (friction / restitution), applied to a bound `UsdShadeMaterial`.
pub const PHYSICS_MATERIAL_SCHEMA: &str = "PhysicsMaterialAPI";

/// `bool` — `physics:rigidBodyEnabled` on `PhysicsRigidBodyAPI` (default `true`).
pub const ATTR_PHYSICS_RIGID_BODY_ENABLED: &str = "physics:rigidBodyEnabled";
/// `bool` — `physics:kinematicEnabled` on `PhysicsRigidBodyAPI` (default
/// `false`): an animator-driven body that pushes dynamics but is not integrated.
pub const ATTR_PHYSICS_KINEMATIC_ENABLED: &str = "physics:kinematicEnabled";
/// `bool` — `physics:startsAsleep` on `PhysicsRigidBodyAPI` (default `false`): a
/// dynamic body begins asleep (does not integrate until woken by a contact or a
/// grab release). The runtime also authors this back so a settled body's USD
/// rest state tracks the live sim (plan Phase 2A).
pub const ATTR_PHYSICS_STARTS_ASLEEP: &str = "physics:startsAsleep";
/// `vector3f` — `physics:velocity` on `PhysicsRigidBodyAPI`: initial linear
/// velocity (stage units/s).
pub const ATTR_PHYSICS_VELOCITY: &str = "physics:velocity";
/// `vector3f` — `physics:angularVelocity` on `PhysicsRigidBodyAPI`: initial
/// angular velocity (**degrees**/s per the spec — convert to rad/s).
pub const ATTR_PHYSICS_ANGULAR_VELOCITY: &str = "physics:angularVelocity";

/// `float` — `physics:mass` on `PhysicsMassAPI` (kg). Highest-precedence mass
/// source (plan §5.3).
pub const ATTR_PHYSICS_MASS: &str = "physics:mass";
/// `float` — `physics:density` on `PhysicsMassAPI` (kg per volume unit³).
pub const ATTR_PHYSICS_DENSITY: &str = "physics:density";

/// `float` — `physics:dynamicFriction` on `PhysicsMaterialAPI`.
pub const ATTR_PHYSICS_DYNAMIC_FRICTION: &str = "physics:dynamicFriction";
/// `float` — `physics:staticFriction` on `PhysicsMaterialAPI`.
pub const ATTR_PHYSICS_STATIC_FRICTION: &str = "physics:staticFriction";
/// `float` — `physics:restitution` on `PhysicsMaterialAPI`.
pub const ATTR_PHYSICS_RESTITUTION: &str = "physics:restitution";

/// USD type name of the `OpenUSD`-standard `UsdPhysicsCollisionAPI` single-apply
/// schema — the gate that marks a gprim collidable.
pub const PHYSICS_COLLISION_SCHEMA: &str = "PhysicsCollisionAPI";
/// USD type name of the `OpenUSD`-standard `UsdPhysicsMeshCollisionAPI` schema
/// (applies only to a `UsdGeomMesh`); carries [`ATTR_PHYSICS_APPROXIMATION`].
pub const PHYSICS_MESH_COLLISION_SCHEMA: &str = "PhysicsMeshCollisionAPI";

/// `bool` — `physics:collisionEnabled` on `PhysicsCollisionAPI` (default
/// `true`). Replaces the retired `idtx:collision:enabled`.
pub const ATTR_PHYSICS_COLLISION_ENABLED: &str = "physics:collisionEnabled";
/// `uniform token` — `physics:approximation` on `PhysicsMeshCollisionAPI`
/// ([`Approximation`]). Replaces the retired `idtx:collision:shape`.
pub const ATTR_PHYSICS_APPROXIMATION: &str = "physics:approximation";

// ── reduced IDTX addendum attributes (gap-fillers over the standard) ─────────

/// `point3f[]` — pre-baked convex-hull vertices in collider-local space, used
/// verbatim when `physics:approximation = convexHull` (never regenerated).
pub const ATTR_HULL_POINTS: &str = "idtx:collision:hull:points";
/// `int[]` — flat triangle index list (3 indices/triangle) into
/// [`ATTR_HULL_POINTS`].
pub const ATTR_HULL_TRIANGLES: &str = "idtx:collision:hull:triangles";
/// `uniform token[]` — IDTX query channels the collider answers
/// ([`CollisionChannel`]); no `OpenUSD`-standard equivalent.
pub const ATTR_CHANNEL: &str = "idtx:collision:channel";

// ── HUD schema / attribute identifiers (IsA concrete typed `IdtxHud`) ────────

/// USD type name of the HUD `IsA` schema. A behavior defines a prim of this type
/// as a child of a camera (into the session layer) to attach a screen-space HUD.
pub const HUD_SCHEMA: &str = "IdtxHud";

/// Property namespace shared by every `IdtxHud` attribute.
pub const HUD_NS: &str = "idtx:hud";

/// `asset` — the HUD image (e.g. a crosshair). Native pixel size is read at load.
pub const ATTR_HUD_IMAGE: &str = "idtx:hud:image";
/// `int2` — on-screen size in pixels; `(0, 0)` means use the image's native size.
pub const ATTR_HUD_SIZE_PX: &str = "idtx:hud:sizePx";
/// `uniform token` — on-screen anchor ([`HudAnchor`]); default `center`.
pub const ATTR_HUD_ANCHOR: &str = "idtx:hud:anchor";
/// `float` — opacity multiplier in `[0, 1]`; default `1.0`.
pub const ATTR_HUD_OPACITY: &str = "idtx:hud:opacity";

// ── behavior schema / attribute identifiers (singleApply `IdtxBehaviorAPI`) ──

/// USD type name of the behavior `HasAPI` schema. Its presence on a prim
/// ([`idtx_usd::StageService::prim_has_api`]) is the canonical "this prim has a
/// behavior" test; all behavior data lives under the [`BEHAVIOR_NS`] namespace.
pub const BEHAVIOR_SCHEMA: &str = "IdtxBehaviorAPI";

/// Property namespace shared by every `IdtxBehaviorAPI` attribute.
pub const BEHAVIOR_NS: &str = "idtx:behavior";

/// `token` — execution profile ([`ATTR_BEHAVIOR_KIND`]); `runtime` (default) or
/// `editor`. Matches `idtx_behavior_abi::BehaviorKind::as_token`.
pub const ATTR_BEHAVIOR_KIND: &str = "idtx:behavior:kind";
/// `string` — behavior display name (the `[behavior].name`).
pub const ATTR_BEHAVIOR_NAME: &str = "idtx:behavior:name";
/// `string` — semantic version string (the `[behavior].version`).
pub const ATTR_BEHAVIOR_VERSION: &str = "idtx:behavior:version";
/// `bool` — editor behaviors: include in the packaged runtime? (default `false`).
pub const ATTR_BEHAVIOR_SHIP: &str = "idtx:behavior:ship";
/// `asset` — authoring Rust source (`.rs`) the behavior is edited as.
pub const ATTR_BEHAVIOR_SOURCE: &str = "idtx:behavior:source";
/// `asset` — compiled WASM module (`.wasm`) the runtime loads; absent for a
/// source-only behavior. A resolvable module is what gates runtime ticking.
pub const ATTR_BEHAVIOR_MODULE: &str = "idtx:behavior:module";
/// `string` — stable module uuid keying the behavior host registry.
pub const ATTR_BEHAVIOR_UUID: &str = "idtx:behavior:uuid";
/// `bool` — whether the behavior ticks (default `true`).
pub const ATTR_BEHAVIOR_ENABLED: &str = "idtx:behavior:enabled";
/// `token` — binding scope ([`ATTR_BEHAVIOR_SCOPE`]); `entity` (default) or
/// `stage`.
pub const ATTR_BEHAVIOR_SCOPE: &str = "idtx:behavior:scope";
/// `bool` — server-authoritative opt-in (default `false`).
pub const ATTR_BEHAVIOR_SERVER_AUTHORITATIVE: &str = "idtx:behavior:serverAuthoritative";
/// `uniform token[]` — granted capability buckets (bucket granted iff its token
/// is present). Values are drawn from [`BEHAVIOR_CAPABILITY_TOKENS`].
pub const ATTR_BEHAVIOR_CAPABILITIES: &str = "idtx:behavior:capabilities";
/// `uniform token[]` — declared input actions, one `"<action>:<kind>"` token per
/// entry (kind ∈ `button`/`axis`/`axis2`).
pub const ATTR_BEHAVIOR_INPUTS: &str = "idtx:behavior:inputs";
/// `int` — optional per-behavior linear-memory cap in MiB; clamp-only. Absent =
/// host default.
pub const ATTR_BEHAVIOR_MEMORY_MB: &str = "idtx:behavior:limits:memoryMb";
/// Prefix for per-parameter attributes (`idtx:behavior:param:<name>`).
pub const ATTR_BEHAVIOR_PARAM_PREFIX: &str = "idtx:behavior:param:";

/// Allowed values of [`ATTR_BEHAVIOR_KIND`], in `allowedTokens` order. Spellings
/// must match `idtx_behavior_abi::BehaviorKind::as_token`.
pub const BEHAVIOR_KIND_TOKENS: [&str; 2] = ["runtime", "editor"];
/// Allowed values of [`ATTR_BEHAVIOR_SCOPE`], in `allowedTokens` order.
pub const BEHAVIOR_SCOPE_TOKENS: [&str; 2] = ["entity", "stage"];
/// Allowed capability tokens for [`ATTR_BEHAVIOR_CAPABILITIES`], in
/// `allowedTokens` order. Spellings must match the host `KNOWN_CAPABILITIES`
/// (`idtx_behavior::manifest`).
pub const BEHAVIOR_CAPABILITY_TOKENS: [&str; 13] = [
    "input",
    "scene_query",
    "xform_write",
    "stage_edit",
    "events",
    "xr_hmd",
    "playback",
    "overlay",
    "physics",
    "auth_write",
    "net",
    "widget",
    "level_control",
];
/// Allowed input-kind tokens used in the `"<action>:<kind>"` encoding of
/// [`ATTR_BEHAVIOR_INPUTS`]. Must match `idtx_behavior::manifest::InputKind`
/// (serde `rename_all = "lowercase"`).
pub const BEHAVIOR_INPUT_KIND_TOKENS: [&str; 3] = ["button", "axis", "axis2"];

// ── collision vocabulary (pure Rust; no USD link required) ──────────────────

/// The **resolved** narrow-phase collider shape. No longer an authored token —
/// it is derived at hydration from the gprim type and `physics:approximation`
/// (see `idtx_scene::hydrator`). The analytic shapes have fast collision tests;
/// [`CollisionShape::Hull`] uses the reduced-schema `hull:*` geometry and
/// [`CollisionShape::Mesh`] uses the Mesh prim the schema is applied to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CollisionShape {
    Sphere,
    Cube,
    Cylinder,
    Cone,
    Capsule,
    Hull,
    Mesh,
}

impl CollisionShape {
    /// Every variant, in `allowedTokens` order — handy for tests and UI.
    pub const ALL: [CollisionShape; 7] = [
        CollisionShape::Sphere,
        CollisionShape::Cube,
        CollisionShape::Cylinder,
        CollisionShape::Cone,
        CollisionShape::Capsule,
        CollisionShape::Hull,
        CollisionShape::Mesh,
    ];

    /// The USD token authored for this shape.
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            CollisionShape::Sphere => "sphere",
            CollisionShape::Cube => "cube",
            CollisionShape::Cylinder => "cylinder",
            CollisionShape::Cone => "cone",
            CollisionShape::Capsule => "capsule",
            CollisionShape::Hull => "hull",
            CollisionShape::Mesh => "mesh",
        }
    }

    /// Parse a USD token into a shape, or `None` if it is not an allowed value.
    #[must_use]
    pub fn from_token(token: &str) -> Option<CollisionShape> {
        CollisionShape::ALL
            .into_iter()
            .find(|shape| shape.as_token() == token)
    }
}

/// Values of [`ATTR_PHYSICS_APPROXIMATION`] (`physics:approximation` on
/// `UsdPhysicsMeshCollisionAPI`). Token spellings are the authoritative ones
/// from the vendored `OpenUSD` `meshCollisionAPI.h` — note `boundingCube` /
/// `boundingSphere` / `meshSimplification` (the `openusd.org` doc page
/// misspells some of these; do not use those).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Approximation {
    /// Collide against the mesh itself (the standard default).
    #[default]
    None,
    ConvexDecomposition,
    ConvexHull,
    BoundingSphere,
    BoundingCube,
    MeshSimplification,
}

impl Approximation {
    /// Every variant, in `allowedTokens` order.
    pub const ALL: [Approximation; 6] = [
        Approximation::None,
        Approximation::ConvexDecomposition,
        Approximation::ConvexHull,
        Approximation::BoundingSphere,
        Approximation::BoundingCube,
        Approximation::MeshSimplification,
    ];

    /// The USD token authored for this approximation.
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            Approximation::None => "none",
            Approximation::ConvexDecomposition => "convexDecomposition",
            Approximation::ConvexHull => "convexHull",
            Approximation::BoundingSphere => "boundingSphere",
            Approximation::BoundingCube => "boundingCube",
            Approximation::MeshSimplification => "meshSimplification",
        }
    }

    /// Parse a USD token into an approximation, or `None` if not allowed.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Approximation> {
        Approximation::ALL
            .into_iter()
            .find(|approx| approx.as_token() == token)
    }
}

/// Allowed values of [`ATTR_CHANNEL`]. [`CollisionChannel::All`] responds to
/// every channel; otherwise a shape lists the specific channels it answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CollisionChannel {
    All,
    Physics,
    Interaction,
}

impl CollisionChannel {
    /// Every variant, in `allowedTokens` order.
    pub const ALL: [CollisionChannel; 3] = [
        CollisionChannel::All,
        CollisionChannel::Physics,
        CollisionChannel::Interaction,
    ];

    /// The USD token authored for this channel.
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            CollisionChannel::All => "All",
            CollisionChannel::Physics => "Physics",
            CollisionChannel::Interaction => "Interaction",
        }
    }

    /// Parse a USD token into a channel, or `None` if it is not allowed.
    #[must_use]
    pub fn from_token(token: &str) -> Option<CollisionChannel> {
        CollisionChannel::ALL
            .into_iter()
            .find(|channel| channel.as_token() == token)
    }
}

/// The long axis of a `cylinder`/`cone`/`capsule` collider, resolved from the
/// collider gprim's own `axis` token (`UsdGeomCylinder`/`Cone`/`Capsule`).
/// Defaults to [`ShapeAxis::Y`] (matching the collider math's local-Y frame).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ShapeAxis {
    X,
    #[default]
    Y,
    Z,
}

impl ShapeAxis {
    /// Every variant, X/Y/Z order.
    pub const ALL: [ShapeAxis; 3] = [ShapeAxis::X, ShapeAxis::Y, ShapeAxis::Z];

    /// The USD token authored for this axis.
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            ShapeAxis::X => "X",
            ShapeAxis::Y => "Y",
            ShapeAxis::Z => "Z",
        }
    }

    /// Parse a USD axis token, or `None` if it is not `X`/`Y`/`Z`.
    #[must_use]
    pub fn from_token(token: &str) -> Option<ShapeAxis> {
        ShapeAxis::ALL
            .into_iter()
            .find(|axis| axis.as_token() == token)
    }
}

/// Allowed values of [`ATTR_HUD_ANCHOR`] — where a HUD element sits on screen.
/// [`HudAnchor::Center`] is the crosshair default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HudAnchor {
    #[default]
    Center,
    Top,
    Bottom,
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl HudAnchor {
    /// Every variant, in `allowedTokens` order.
    pub const ALL: [HudAnchor; 9] = [
        HudAnchor::Center,
        HudAnchor::Top,
        HudAnchor::Bottom,
        HudAnchor::Left,
        HudAnchor::Right,
        HudAnchor::TopLeft,
        HudAnchor::TopRight,
        HudAnchor::BottomLeft,
        HudAnchor::BottomRight,
    ];

    /// The USD token authored for this anchor.
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            HudAnchor::Center => "center",
            HudAnchor::Top => "top",
            HudAnchor::Bottom => "bottom",
            HudAnchor::Left => "left",
            HudAnchor::Right => "right",
            HudAnchor::TopLeft => "topLeft",
            HudAnchor::TopRight => "topRight",
            HudAnchor::BottomLeft => "bottomLeft",
            HudAnchor::BottomRight => "bottomRight",
        }
    }

    /// Parse a USD token into an anchor, or `None` if it is not an allowed value.
    #[must_use]
    pub fn from_token(token: &str) -> Option<HudAnchor> {
        HudAnchor::ALL
            .into_iter()
            .find(|anchor| anchor.as_token() == token)
    }
}

// ── widget schema / attribute identifiers (IsA typed `IdtxWidget*`) ──────────
//
// Native-USD 2D UI in 3D space.
// `IdtxWidgetPanel` derives from `UsdGeomGprim` — it is the ONE widget prim that
// enters real 3D space, rendered as an offscreen-texture quad. Every element prim
// derives from the abstract `IdtxWidgetElement` (a `UsdGeomImageable` — a pure
// visual with no independent transform or bounds) and is composited into the
// panel's 2D texture; the panel lays elements out itself using the 2D
// [`ATTR_WIDGET_POSITION`] / [`ATTR_WIDGET_ORDER`] attributes the base declares.
// All properties live under the [`WIDGET_NS`] namespace. The bridge resolves
// these codeless types by string (`prim_is_a` / `eval_attribute`) with no
// `idtx-usd-sys` change.

/// USD type name of the widget surface `IsA` schema (derives from
/// `UsdGeomGprim`). `prim_is_a(prim, WIDGET_PANEL_SCHEMA)` is the canonical
/// "this prim is a widget panel" test.
pub const WIDGET_PANEL_SCHEMA: &str = "IdtxWidgetPanel";
/// USD type name of the **abstract** element base (`abstractTyped`, derives from
/// `UsdGeomImageable`; not instantiable). Every element type below is an
/// `IdtxWidgetElement` and inherits [`ATTR_WIDGET_POSITION`] / [`ATTR_WIDGET_ORDER`].
pub const WIDGET_ELEMENT_SCHEMA: &str = "IdtxWidgetElement";
/// USD type name of the layout-container element schema.
pub const WIDGET_CONTAINER_SCHEMA: &str = "IdtxWidgetContainer";
/// USD type name of the text-label element schema.
pub const WIDGET_LABEL_SCHEMA: &str = "IdtxWidgetLabel";
/// USD type name of the button element schema.
pub const WIDGET_BUTTON_SCHEMA: &str = "IdtxWidgetButton";
/// USD type name of the image element schema.
pub const WIDGET_IMAGE_SCHEMA: &str = "IdtxWidgetImage";
/// USD type name of the scalar-slider element schema.
pub const WIDGET_SLIDER_SCHEMA: &str = "IdtxWidgetSlider";
/// USD type name of the boolean-toggle element schema.
pub const WIDGET_TOGGLE_SCHEMA: &str = "IdtxWidgetToggle";
/// USD type name of the **abstract** value-field base (`abstractTyped`; not
/// instantiable). Concrete, value-typed subclasses (e.g.
/// [`WIDGET_FIELD_DOUBLE_SCHEMA`]) carry the bindable `idtx:widget:value`.
pub const WIDGET_FIELD_SCHEMA: &str = "IdtxWidgetField";
/// USD type name of the concrete `double`-valued field element — the bindable
/// readout whose `double idtx:widget:value` connects to an `openExec` compute
/// output. Placed beside an [`WIDGET_LABEL_SCHEMA`] (typically the next column).
pub const WIDGET_FIELD_DOUBLE_SCHEMA: &str = "IdtxWidgetFieldDouble";
/// USD type name of the concrete `float`-valued field element.
pub const WIDGET_FIELD_FLOAT_SCHEMA: &str = "IdtxWidgetFieldFloat";
/// USD type name of the concrete `int`-valued field element.
pub const WIDGET_FIELD_INT_SCHEMA: &str = "IdtxWidgetFieldInt";
/// USD type name of the concrete `string`-valued field element.
pub const WIDGET_FIELD_STRING_SCHEMA: &str = "IdtxWidgetFieldString";

/// Every **instantiable** widget prim type name, panel first then elements —
/// handy for populator `type_names` registration and tests. The abstract
/// [`WIDGET_FIELD_SCHEMA`] base is intentionally excluded (it has no instances).
pub const WIDGET_SCHEMAS: [&str; 11] = [
    WIDGET_PANEL_SCHEMA,
    WIDGET_CONTAINER_SCHEMA,
    WIDGET_LABEL_SCHEMA,
    WIDGET_BUTTON_SCHEMA,
    WIDGET_IMAGE_SCHEMA,
    WIDGET_SLIDER_SCHEMA,
    WIDGET_TOGGLE_SCHEMA,
    WIDGET_FIELD_DOUBLE_SCHEMA,
    WIDGET_FIELD_FLOAT_SCHEMA,
    WIDGET_FIELD_INT_SCHEMA,
    WIDGET_FIELD_STRING_SCHEMA,
];

/// Property namespace shared by every `IdtxWidget*` attribute.
pub const WIDGET_NS: &str = "idtx:widget";

// Panel (`IdtxWidgetPanel`) attributes.
/// `float2` — physical panel size `(width, height)` in world units.
pub const ATTR_WIDGET_SIZE: &str = "idtx:widget:size";
/// `int2` — offscreen rasterization size in pixels; default `(512, 512)`.
pub const ATTR_WIDGET_RESOLUTION: &str = "idtx:widget:resolution";
/// `float` — rasterization density (pixels per world unit); default `2048`.
pub const ATTR_WIDGET_PIXELS_PER_UNIT: &str = "idtx:widget:pixelsPerUnit";
/// `uniform token` — camera-facing mode ([`WidgetBillboard`]); default `none`.
pub const ATTR_WIDGET_BILLBOARD: &str = "idtx:widget:billboard";
/// `uniform bool` — render both faces; default `true`.
pub const ATTR_WIDGET_DOUBLE_SIDED: &str = "idtx:widget:doubleSided";
/// `float` — panel opacity multiplier in `[0, 1]`; default `1.0`.
pub const ATTR_WIDGET_OPACITY: &str = "idtx:widget:opacity";
/// `uniform token` — compositing space ([`WidgetSpace`]); default `world`.
pub const ATTR_WIDGET_SPACE: &str = "idtx:widget:space";
/// `asset` — opaque imported-UI escape hatch; when set, child elements ignored.
pub const ATTR_WIDGET_CONTENT_SOURCE: &str = "idtx:widget:contentSource";

// Element base (`IdtxWidgetElement`) attributes — shared by every element.
/// `float2` — panel-local NORMALIZED position `(0,0)` = top-left … `(1,1)` =
/// bottom-right; default `(0, 0)` meaning "unpositioned" (fall back to
/// namespace-order layout).
pub const ATTR_WIDGET_POSITION: &str = "idtx:widget:position";
/// `int` — paint/stacking order within the panel's 2D texture (higher draws on
/// top); default `0`. The 2D stand-in for a UI layer — carries no 3D depth.
pub const ATTR_WIDGET_ORDER: &str = "idtx:widget:order";

// Container (`IdtxWidgetContainer`) attributes.
/// `uniform token` — child arrangement ([`WidgetLayout`]); default `column`.
pub const ATTR_WIDGET_LAYOUT: &str = "idtx:widget:layout";
/// `float` — spacing between children in pixels.
pub const ATTR_WIDGET_GAP: &str = "idtx:widget:gap";
/// `float` — inner padding around children in pixels.
pub const ATTR_WIDGET_PADDING: &str = "idtx:widget:padding";
/// `uniform token` — cross-axis alignment ([`WidgetAlignment`]); default `start`.
pub const ATTR_WIDGET_ALIGNMENT: &str = "idtx:widget:alignment";

// Label (`IdtxWidgetLabel`) attributes.
/// `string` — the displayed text.
pub const ATTR_WIDGET_TEXT: &str = "idtx:widget:text";
/// `float` — font size in points; default `16`.
pub const ATTR_WIDGET_FONT_SIZE: &str = "idtx:widget:fontSize";
/// `color3f` — foreground text color (linear RGB); default `(1, 1, 1)`.
pub const ATTR_WIDGET_COLOR: &str = "idtx:widget:color";
/// `asset` — font `.ttf`/`.otf` resolved through the USD asset resolver
/// (usdz-packageable); absent = use [`ATTR_WIDGET_FONT_FAMILY`] / the default.
pub const ATTR_WIDGET_FONT: &str = "idtx:widget:font";
/// `token` — named font-family fallback (open-ended; no `allowedTokens`).
pub const ATTR_WIDGET_FONT_FAMILY: &str = "idtx:widget:fontFamily";

// Button (`IdtxWidgetButton`) attributes.
/// `string` — button caption text.
pub const ATTR_WIDGET_LABEL: &str = "idtx:widget:label";
/// `bool` — whether the button accepts clicks; default `true`.
pub const ATTR_WIDGET_ENABLED: &str = "idtx:widget:enabled";

// Image (`IdtxWidgetImage`) attributes.
/// `asset` — the image asset to display.
pub const ATTR_WIDGET_IMAGE: &str = "idtx:widget:image";
/// `uniform token` — how the image is scaled ([`WidgetFit`]); default `contain`.
pub const ATTR_WIDGET_FIT: &str = "idtx:widget:fit";

// Slider (`IdtxWidgetSlider`) / Toggle (`IdtxWidgetToggle`) attributes.
/// `float` (slider) / `bool` (toggle) — current value. One attribute name, two
/// value types scoped by the owning prim type.
pub const ATTR_WIDGET_VALUE: &str = "idtx:widget:value";
/// `float` — slider minimum; default `0`.
pub const ATTR_WIDGET_MIN: &str = "idtx:widget:min";
/// `float` — slider maximum; default `1`.
pub const ATTR_WIDGET_MAX: &str = "idtx:widget:max";

// Field (`IdtxWidgetField*`) attributes.
/// `int` — fractional digits used when formatting a float/double field value
/// (`IdtxWidgetFieldDouble` / `IdtxWidgetFieldFloat` only).
pub const ATTR_WIDGET_PRECISION: &str = "idtx:widget:precision";

/// Allowed values of [`ATTR_WIDGET_BILLBOARD`] — the camera-facing mode of a
/// panel. [`WidgetBillboard::None`] keeps the authored orientation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WidgetBillboard {
    #[default]
    None,
    ScreenAligned,
    YAxis,
}

impl WidgetBillboard {
    /// Every variant, in `allowedTokens` order.
    pub const ALL: [WidgetBillboard; 3] = [
        WidgetBillboard::None,
        WidgetBillboard::ScreenAligned,
        WidgetBillboard::YAxis,
    ];

    /// The USD token authored for this mode.
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            WidgetBillboard::None => "none",
            WidgetBillboard::ScreenAligned => "screenAligned",
            WidgetBillboard::YAxis => "yAxis",
        }
    }

    /// Parse a USD token, or `None` if it is not an allowed value.
    #[must_use]
    pub fn from_token(token: &str) -> Option<WidgetBillboard> {
        WidgetBillboard::ALL
            .into_iter()
            .find(|mode| mode.as_token() == token)
    }
}

/// Allowed values of [`ATTR_WIDGET_SPACE`] — whether a panel draws in-scene or
/// screen-space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WidgetSpace {
    #[default]
    World,
    Hud,
}

impl WidgetSpace {
    /// Every variant, in `allowedTokens` order.
    pub const ALL: [WidgetSpace; 2] = [WidgetSpace::World, WidgetSpace::Hud];

    /// The USD token authored for this space.
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            WidgetSpace::World => "world",
            WidgetSpace::Hud => "hud",
        }
    }

    /// Parse a USD token, or `None` if it is not an allowed value.
    #[must_use]
    pub fn from_token(token: &str) -> Option<WidgetSpace> {
        WidgetSpace::ALL
            .into_iter()
            .find(|space| space.as_token() == token)
    }
}

/// Allowed values of [`ATTR_WIDGET_LAYOUT`] — a container's child arrangement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WidgetLayout {
    Row,
    #[default]
    Column,
    Grid,
    Stack,
}

impl WidgetLayout {
    /// Every variant, in `allowedTokens` order.
    pub const ALL: [WidgetLayout; 4] = [
        WidgetLayout::Row,
        WidgetLayout::Column,
        WidgetLayout::Grid,
        WidgetLayout::Stack,
    ];

    /// The USD token authored for this layout.
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            WidgetLayout::Row => "row",
            WidgetLayout::Column => "column",
            WidgetLayout::Grid => "grid",
            WidgetLayout::Stack => "stack",
        }
    }

    /// Parse a USD token, or `None` if it is not an allowed value.
    #[must_use]
    pub fn from_token(token: &str) -> Option<WidgetLayout> {
        WidgetLayout::ALL
            .into_iter()
            .find(|layout| layout.as_token() == token)
    }
}

/// Allowed values of [`ATTR_WIDGET_ALIGNMENT`] — cross-axis alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WidgetAlignment {
    #[default]
    Start,
    Center,
    End,
    Stretch,
}

impl WidgetAlignment {
    /// Every variant, in `allowedTokens` order.
    pub const ALL: [WidgetAlignment; 4] = [
        WidgetAlignment::Start,
        WidgetAlignment::Center,
        WidgetAlignment::End,
        WidgetAlignment::Stretch,
    ];

    /// The USD token authored for this alignment.
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            WidgetAlignment::Start => "start",
            WidgetAlignment::Center => "center",
            WidgetAlignment::End => "end",
            WidgetAlignment::Stretch => "stretch",
        }
    }

    /// Parse a USD token, or `None` if it is not an allowed value.
    #[must_use]
    pub fn from_token(token: &str) -> Option<WidgetAlignment> {
        WidgetAlignment::ALL
            .into_iter()
            .find(|align| align.as_token() == token)
    }
}

/// Allowed values of [`ATTR_WIDGET_FIT`] — how an image scales within its box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WidgetFit {
    Fill,
    #[default]
    Contain,
    Cover,
    None,
}

impl WidgetFit {
    /// Every variant, in `allowedTokens` order.
    pub const ALL: [WidgetFit; 4] = [
        WidgetFit::Fill,
        WidgetFit::Contain,
        WidgetFit::Cover,
        WidgetFit::None,
    ];

    /// The USD token authored for this fit.
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            WidgetFit::Fill => "fill",
            WidgetFit::Contain => "contain",
            WidgetFit::Cover => "cover",
            WidgetFit::None => "none",
        }
    }

    /// Parse a USD token, or `None` if it is not an allowed value.
    #[must_use]
    pub fn from_token(token: &str) -> Option<WidgetFit> {
        WidgetFit::ALL
            .into_iter()
            .find(|fit| fit.as_token() == token)
    }
}

// ── plugin directory resolution + registration ──────────────────────────────

/// Directory holding the schema `plugInfo.json` + `generatedSchema.usda`, baked
/// in at compile time by `build.rs` (points into `OUT_DIR`). Used for `cargo run`
/// and tests; a deployed bundle is handled by [`plugin_dir`].
const BAKED_PLUGIN_DIR: &str = env!("IDTX_USD_EXTENSION_PLUGIN_DIR");

/// Resolve the directory that holds the schema `plugInfo.json` at runtime.
///
/// In a macOS `.app` bundle the executable sits at
/// `<App>.app/Contents/MacOS/<exe>` and the schema resources are packaged at
/// `<App>.app/Contents/Resources/idtx-usd-extension/`. If that path exists we
/// return it so the bundle is self-contained; otherwise we fall back to
/// [`BAKED_PLUGIN_DIR`] (the `OUT_DIR` baked in at compile time).
///
/// This mirrors the HTTP resolver's `resolve_plugin_dir`
/// (`crates/idtx-usd-resolver-http/src/lib.rs`).
#[must_use]
pub fn plugin_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(macos_dir) = exe.parent() {
            if macos_dir.file_name().is_some_and(|n| n == "MacOS") {
                if let Some(contents) = macos_dir.parent() {
                    let bundled = contents.join("Resources").join("idtx-usd-extension");
                    if bundled.join("plugInfo.json").exists() {
                        return bundled;
                    }
                }
            }
        }
    }
    PathBuf::from(BAKED_PLUGIN_DIR)
}

/// Register the IDTX USD schema plugin ([`COLLISION_SCHEMA`] and future schemas)
/// with the `OpenUSD` plugin registry.
///
/// **Call once, before opening any stage that uses the schema.** Registration
/// goes through [`idtx_usd::register_plugin_path`] →
/// `PlugRegistry::RegisterPlugins`, which is the only reliable runtime path:
/// `PXR_PLUGINPATH_NAME` is read by `OpenUSD` at library-load time, so setting it
/// after process start is too late (see the `project_usd_resolver_plugin_discovery`
/// note).
///
/// Idempotent: after the first successful registration, further calls are no-ops
/// and return `Ok(())`. This avoids re-adding the schema `TfType` alias — which
/// `OpenUSD` rejects with a (non-fatal) coding error — so a host that cannot
/// easily thread a "registered" flag may call this freely at each stage open.
///
/// # Errors
/// Returns [`StageError`] if the plugin directory path is non-UTF-8 or `OpenUSD`
/// rejects the registration.
pub fn install() -> Result<(), StageError> {
    register_from(&plugin_dir())
}

/// Register the IDTX USD schema plugin from an **explicit** directory holding the
/// staged `plugInfo.json` + `generatedSchema.usda` — a cooked payload's
/// `usd-schemas/` dir — rather than the compile-time [`BAKED_PLUGIN_DIR`].
///
/// A packaged player runs on a device / machine where the baked `OUT_DIR` path
/// recorded at build time does not exist, so [`plugin_dir`]'s fallback points at
/// nothing and [`install`] silently registers an empty directory (no schema → no
/// `IdtxCollisionAPI` → collision never hydrates → collidable-only raycasts find
/// nothing). The runtime stages the schema resources beside the payload and calls
/// this with the on-disk location instead.
///
/// Shares [`install`]'s idempotency latch: the first successful registration (by
/// either entry point) wins and later calls are no-ops, so a host may call this
/// then `install` (or vice versa) without re-adding the `TfType` alias.
///
/// # Errors
/// Returns [`StageError`] if the plugin directory path is non-UTF-8 or `OpenUSD`
/// rejects the registration.
pub fn install_at(dir: impl AsRef<std::path::Path>) -> Result<(), StageError> {
    register_from(dir.as_ref())
}

/// Shared registration body for [`install`] / [`install_at`]: idempotent
/// `PlugRegistry::RegisterPlugins` of `dir`.
fn register_from(dir: &std::path::Path) -> Result<(), StageError> {
    use std::sync::atomic::{AtomicBool, Ordering};
    // Set only after a successful registration, so a failed first attempt can be
    // retried. install() is a documented startup one-shot, so the brief window
    // where two threads could both register before the flag is set is benign.
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.load(Ordering::Acquire) {
        return Ok(());
    }

    #[cfg(feature = "metrics")]
    let start = std::time::Instant::now();

    idtx_usd::register_plugin_path(dir)?;
    REGISTERED.store(true, Ordering::Release);

    #[cfg(feature = "metrics")]
    tracing::info!(
        plugin_dir = %dir.display(),
        ms = start.elapsed().as_secs_f64() * 1e3,
        "registered IDTX USD schema plugin"
    );
    #[cfg(not(feature = "metrics"))]
    tracing::debug!(plugin_dir = %dir.display(), "registered IDTX USD schema plugin");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physics_schema_and_attribute_spellings() {
        // The physics-scene gate and rigid-body/mass/material schema names must
        // match the usdPhysics plugin's exact type names (no `Usd` prefix in the
        // `apiSchemas`/`typeName` tokens `prim_has_api` / `type_name` compare).
        assert_eq!(PHYSICS_SCENE_TYPE, "PhysicsScene");
        assert_eq!(PHYSICS_RIGID_BODY_SCHEMA, "PhysicsRigidBodyAPI");
        assert_eq!(PHYSICS_MASS_SCHEMA, "PhysicsMassAPI");
        assert_eq!(PHYSICS_MATERIAL_SCHEMA, "PhysicsMaterialAPI");
        // Every physics attribute lives under the OpenUSD `physics` namespace.
        for attr in [
            ATTR_PHYSICS_GRAVITY_DIRECTION,
            ATTR_PHYSICS_GRAVITY_MAGNITUDE,
            ATTR_PHYSICS_RIGID_BODY_ENABLED,
            ATTR_PHYSICS_KINEMATIC_ENABLED,
            ATTR_PHYSICS_STARTS_ASLEEP,
            ATTR_PHYSICS_VELOCITY,
            ATTR_PHYSICS_ANGULAR_VELOCITY,
            ATTR_PHYSICS_MASS,
            ATTR_PHYSICS_DENSITY,
            ATTR_PHYSICS_DYNAMIC_FRICTION,
            ATTR_PHYSICS_STATIC_FRICTION,
            ATTR_PHYSICS_RESTITUTION,
        ] {
            assert!(attr.starts_with("physics:"), "{attr} must be under physics:");
        }
    }

    #[test]
    fn collision_shape_token_roundtrips_for_all_variants() {
        for shape in CollisionShape::ALL {
            assert_eq!(CollisionShape::from_token(shape.as_token()), Some(shape));
        }
        // Exact token spellings must match the schema's allowedTokens.
        assert_eq!(CollisionShape::Sphere.as_token(), "sphere");
        assert_eq!(CollisionShape::Mesh.as_token(), "mesh");
        // Unknown / wrong-case tokens are rejected.
        assert_eq!(CollisionShape::from_token("Sphere"), None);
        assert_eq!(CollisionShape::from_token("triangle"), None);
        assert_eq!(CollisionShape::from_token(""), None);
    }

    #[test]
    fn collision_channel_token_roundtrips_for_all_variants() {
        for channel in CollisionChannel::ALL {
            assert_eq!(
                CollisionChannel::from_token(channel.as_token()),
                Some(channel)
            );
        }
        assert_eq!(CollisionChannel::All.as_token(), "All");
        assert_eq!(CollisionChannel::Interaction.as_token(), "Interaction");
        // Channel tokens are capitalised; lowercase must not match.
        assert_eq!(CollisionChannel::from_token("all"), None);
        assert_eq!(CollisionChannel::from_token("Collision"), None);
    }

    #[test]
    fn approximation_token_roundtrips_for_all_variants() {
        for approx in Approximation::ALL {
            assert_eq!(Approximation::from_token(approx.as_token()), Some(approx));
        }
        // Exact spellings from the vendored meshCollisionAPI.h allowedTokens.
        assert_eq!(Approximation::None.as_token(), "none");
        assert_eq!(Approximation::ConvexHull.as_token(), "convexHull");
        assert_eq!(Approximation::BoundingCube.as_token(), "boundingCube");
        assert_eq!(Approximation::default(), Approximation::None);
        // Common misspellings / wrong case must be rejected.
        assert_eq!(Approximation::from_token("boundingBox"), None);
        assert_eq!(Approximation::from_token("simplifiedMesh"), None);
        assert_eq!(Approximation::from_token("ConvexHull"), None);
        assert_eq!(Approximation::from_token(""), None);
    }

    #[test]
    fn attribute_constants_are_namespaced_correctly() {
        // The reduced IDTX addendum attributes live under `idtx:collision`.
        for attr in [ATTR_HULL_POINTS, ATTR_HULL_TRIANGLES, ATTR_CHANNEL] {
            assert!(
                attr.starts_with(&format!("{COLLISION_NS}:")),
                "{attr} must be under {COLLISION_NS}"
            );
        }
        assert_eq!(ATTR_HULL_POINTS, "idtx:collision:hull:points");
        assert_eq!(ATTR_HULL_TRIANGLES, "idtx:collision:hull:triangles");
        assert_eq!(ATTR_CHANNEL, "idtx:collision:channel");
        // The collidable gate + enable/approximation live under the OpenUSD
        // `physics` namespace (owned by the standard usdPhysics schemas).
        assert_eq!(PHYSICS_COLLISION_SCHEMA, "PhysicsCollisionAPI");
        assert_eq!(PHYSICS_MESH_COLLISION_SCHEMA, "PhysicsMeshCollisionAPI");
        assert_eq!(ATTR_PHYSICS_COLLISION_ENABLED, "physics:collisionEnabled");
        assert_eq!(ATTR_PHYSICS_APPROXIMATION, "physics:approximation");
    }

    #[test]
    fn shape_axis_token_roundtrips_and_defaults_to_y() {
        for axis in ShapeAxis::ALL {
            assert_eq!(ShapeAxis::from_token(axis.as_token()), Some(axis));
        }
        assert_eq!(ShapeAxis::X.as_token(), "X");
        assert_eq!(ShapeAxis::default(), ShapeAxis::Y);
        // Lowercase / unknown tokens are rejected.
        assert_eq!(ShapeAxis::from_token("x"), None);
        assert_eq!(ShapeAxis::from_token("W"), None);
        assert_eq!(ShapeAxis::from_token(""), None);
    }

    /// The baked plugin dir must contain both resource files that `OpenUSD` needs,
    /// and `plugInfo.json` must declare `IdtxCollisionAPI` as a codeless
    /// single-apply API. Guards against build.rs regressions and schema typos.
    #[test]
    fn plug_info_declares_codeless_single_apply_collision_api() {
        let dir = PathBuf::from(BAKED_PLUGIN_DIR);
        let plug_info = dir.join("plugInfo.json");
        let generated = dir.join("generatedSchema.usda");
        assert!(plug_info.exists(), "plugInfo.json missing in baked dir");
        assert!(generated.exists(), "generatedSchema.usda missing in baked dir");

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&plug_info).expect("read plugInfo.json"))
                .expect("plugInfo.json is valid JSON");
        let plugin = &json["Plugins"][0];
        assert_eq!(plugin["Type"], "resource", "codeless => resource plugin");
        let api = &plugin["Info"]["Types"][COLLISION_SCHEMA];
        assert_eq!(api["schemaKind"], "singleApplyAPI");
        assert_eq!(api["bases"][0], "UsdAPISchemaBase");
        assert_eq!(api["schemaIdentifier"], COLLISION_SCHEMA);
    }

    /// The generated schema must define the class and all three surviving
    /// (reduced) namespaced properties with their allowedTokens, so authored
    /// hull/channel content validates.
    #[test]
    fn generated_schema_defines_class_and_all_properties() {
        let text = std::fs::read_to_string(PathBuf::from(BAKED_PLUGIN_DIR).join("generatedSchema.usda"))
            .expect("read generatedSchema.usda");
        assert!(text.contains(&format!("class \"{COLLISION_SCHEMA}\"")));
        for attr in [ATTR_HULL_POINTS, ATTR_HULL_TRIANGLES, ATTR_CHANNEL] {
            assert!(text.contains(attr), "generatedSchema missing {attr}");
        }
        // The retired `shape`/`enabled`/analytic-dim attrs must be GONE.
        assert!(
            !text.contains("idtx:collision:shape"),
            "retired idtx:collision:shape still present in generatedSchema"
        );
        assert!(
            !text.contains("idtx:collision:enabled"),
            "retired idtx:collision:enabled still present in generatedSchema"
        );
        for channel in CollisionChannel::ALL {
            assert!(
                text.contains(&format!("\"{}\"", channel.as_token())),
                "generatedSchema missing channel token {}",
                channel.as_token()
            );
        }
    }

    /// The collision-body marker is a codeless single-apply API, mirroring
    /// `IdtxCollisionAPI` but property-less.
    #[test]
    fn plug_info_declares_codeless_single_apply_collision_body_api() {
        let dir = PathBuf::from(BAKED_PLUGIN_DIR);
        let json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("plugInfo.json")).expect("read plugInfo.json"),
        )
        .expect("plugInfo.json is valid JSON");
        let api = &json["Plugins"][0]["Info"]["Types"][COLLISION_BODY_SCHEMA];
        assert_eq!(api["schemaKind"], "singleApplyAPI");
        assert_eq!(api["bases"][0], "UsdAPISchemaBase");
        assert_eq!(api["schemaIdentifier"], COLLISION_BODY_SCHEMA);
        // Codeless: no per-type `alias` block (usdGenSchema may re-add one).
        assert!(
            api.get("alias").is_none(),
            "codeless schema must not carry a per-type alias block"
        );
    }

    /// The collision-body marker is property-less: the generated schema must
    /// declare the class but author no attributes under it.
    #[test]
    fn generated_schema_defines_empty_collision_body_class() {
        let text = std::fs::read_to_string(
            PathBuf::from(BAKED_PLUGIN_DIR).join("generatedSchema.usda"),
        )
        .expect("read generatedSchema.usda");
        assert!(
            text.contains(&format!("class \"{COLLISION_BODY_SCHEMA}\"")),
            "generatedSchema missing IdtxCollisionBodyAPI class"
        );
    }

    #[test]
    fn hud_anchor_token_roundtrips_for_all_variants() {
        for anchor in HudAnchor::ALL {
            assert_eq!(HudAnchor::from_token(anchor.as_token()), Some(anchor));
        }
        // Exact token spellings must match the schema's allowedTokens.
        assert_eq!(HudAnchor::Center.as_token(), "center");
        assert_eq!(HudAnchor::TopLeft.as_token(), "topLeft");
        assert_eq!(HudAnchor::default(), HudAnchor::Center);
        // Unknown / wrong-case tokens are rejected.
        assert_eq!(HudAnchor::from_token("Center"), None);
        assert_eq!(HudAnchor::from_token("middle"), None);
        assert_eq!(HudAnchor::from_token(""), None);
    }

    #[test]
    fn hud_attribute_constants_are_namespaced_under_hud_ns() {
        for attr in [
            ATTR_HUD_IMAGE,
            ATTR_HUD_SIZE_PX,
            ATTR_HUD_ANCHOR,
            ATTR_HUD_OPACITY,
        ] {
            assert!(
                attr.starts_with(&format!("{HUD_NS}:")),
                "{attr} must be under {HUD_NS}"
            );
        }
        assert_eq!(ATTR_HUD_IMAGE, "idtx:hud:image");
        assert_eq!(ATTR_HUD_SIZE_PX, "idtx:hud:sizePx");
        assert_eq!(ATTR_HUD_ANCHOR, "idtx:hud:anchor");
        assert_eq!(ATTR_HUD_OPACITY, "idtx:hud:opacity");
    }

    /// `plugInfo.json` must declare `IdtxHud` as a codeless concrete typed prim
    /// schema (so `DefinePrim(path, "IdtxHud")` resolves), with NO per-type
    /// `alias` block (the registry adds the alias from generatedSchema.usda;
    /// declaring it twice emits a noisy coding error — see the codeless-schema
    /// memory note).
    #[test]
    fn plug_info_declares_codeless_concrete_typed_hud() {
        let dir = PathBuf::from(BAKED_PLUGIN_DIR);
        let json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("plugInfo.json")).expect("read plugInfo.json"),
        )
        .expect("plugInfo.json is valid JSON");
        let hud = &json["Plugins"][0]["Info"]["Types"][HUD_SCHEMA];
        assert_eq!(hud["schemaKind"], "concreteTyped");
        assert_eq!(hud["bases"][0], "UsdTyped");
        assert_eq!(hud["schemaIdentifier"], HUD_SCHEMA);
        assert!(
            hud.get("alias").is_none(),
            "codeless schema must not declare a per-type alias in plugInfo"
        );
    }

    /// The generated schema must define the HUD class and all its namespaced
    /// properties + anchor allowedTokens, so authored HUD content validates.
    #[test]
    fn generated_schema_defines_hud_class_and_properties() {
        let text =
            std::fs::read_to_string(PathBuf::from(BAKED_PLUGIN_DIR).join("generatedSchema.usda"))
                .expect("read generatedSchema.usda");
        // Concrete typed schemas carry the prim-type identifier before the quoted
        // name (unlike API schemas, which are `class "Name"`).
        assert!(text.contains(&format!("class {HUD_SCHEMA} \"{HUD_SCHEMA}\"")));
        for attr in [
            ATTR_HUD_IMAGE,
            ATTR_HUD_SIZE_PX,
            ATTR_HUD_ANCHOR,
            ATTR_HUD_OPACITY,
        ] {
            assert!(text.contains(attr), "generatedSchema missing {attr}");
        }
        for anchor in HudAnchor::ALL {
            assert!(
                text.contains(&format!("\"{}\"", anchor.as_token())),
                "generatedSchema missing anchor token {}",
                anchor.as_token()
            );
        }
    }

    #[test]
    fn behavior_attribute_constants_are_namespaced_under_behavior_ns() {
        for attr in [
            ATTR_BEHAVIOR_KIND,
            ATTR_BEHAVIOR_NAME,
            ATTR_BEHAVIOR_VERSION,
            ATTR_BEHAVIOR_SHIP,
            ATTR_BEHAVIOR_SOURCE,
            ATTR_BEHAVIOR_MODULE,
            ATTR_BEHAVIOR_UUID,
            ATTR_BEHAVIOR_ENABLED,
            ATTR_BEHAVIOR_SCOPE,
            ATTR_BEHAVIOR_SERVER_AUTHORITATIVE,
            ATTR_BEHAVIOR_CAPABILITIES,
            ATTR_BEHAVIOR_INPUTS,
            ATTR_BEHAVIOR_MEMORY_MB,
        ] {
            assert!(
                attr.starts_with(&format!("{BEHAVIOR_NS}:")),
                "{attr} must be under {BEHAVIOR_NS}"
            );
        }
        assert_eq!(ATTR_BEHAVIOR_KIND, "idtx:behavior:kind");
        assert_eq!(ATTR_BEHAVIOR_NAME, "idtx:behavior:name");
        assert_eq!(ATTR_BEHAVIOR_VERSION, "idtx:behavior:version");
        assert_eq!(ATTR_BEHAVIOR_SHIP, "idtx:behavior:ship");
        assert_eq!(ATTR_BEHAVIOR_SOURCE, "idtx:behavior:source");
        assert_eq!(ATTR_BEHAVIOR_MODULE, "idtx:behavior:module");
        assert_eq!(ATTR_BEHAVIOR_UUID, "idtx:behavior:uuid");
        assert_eq!(ATTR_BEHAVIOR_ENABLED, "idtx:behavior:enabled");
        assert_eq!(ATTR_BEHAVIOR_SCOPE, "idtx:behavior:scope");
        assert_eq!(
            ATTR_BEHAVIOR_SERVER_AUTHORITATIVE,
            "idtx:behavior:serverAuthoritative"
        );
        assert_eq!(ATTR_BEHAVIOR_CAPABILITIES, "idtx:behavior:capabilities");
        assert_eq!(ATTR_BEHAVIOR_INPUTS, "idtx:behavior:inputs");
        assert_eq!(ATTR_BEHAVIOR_MEMORY_MB, "idtx:behavior:limits:memoryMb");
        assert_eq!(ATTR_BEHAVIOR_PARAM_PREFIX, "idtx:behavior:param:");
        // kind token spellings must match idtx_behavior_abi::BehaviorKind.
        assert_eq!(BEHAVIOR_KIND_TOKENS, ["runtime", "editor"]);
        assert_eq!(BEHAVIOR_SCOPE_TOKENS, ["entity", "stage"]);
        // capability tokens must match the host KNOWN_CAPABILITIES set.
        assert_eq!(
            BEHAVIOR_CAPABILITY_TOKENS,
            [
                "input",
                "scene_query",
                "xform_write",
                "stage_edit",
                "events",
                "xr_hmd",
                "playback",
                "overlay",
                "physics",
                "auth_write",
                "net",
                "widget",
                "level_control",
            ]
        );
        assert_eq!(BEHAVIOR_INPUT_KIND_TOKENS, ["button", "axis", "axis2"]);
    }

    /// `plugInfo.json` must declare `IdtxBehaviorAPI` as a codeless single-apply
    /// API (so `ApplyApi`/`HasAPI` resolve), with NO per-type `alias` block (the
    /// registry adds the alias from generatedSchema.usda; declaring it twice
    /// emits a noisy coding error — see the codeless-schema memory note).
    #[test]
    fn plug_info_declares_codeless_single_apply_behavior_api() {
        let dir = PathBuf::from(BAKED_PLUGIN_DIR);
        let json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("plugInfo.json")).expect("read plugInfo.json"),
        )
        .expect("plugInfo.json is valid JSON");
        let api = &json["Plugins"][0]["Info"]["Types"][BEHAVIOR_SCHEMA];
        assert_eq!(api["schemaKind"], "singleApplyAPI");
        assert_eq!(api["bases"][0], "UsdAPISchemaBase");
        assert_eq!(api["schemaIdentifier"], BEHAVIOR_SCHEMA);
        assert!(
            api.get("alias").is_none(),
            "codeless schema must not carry a per-type alias block"
        );
    }

    /// The generated schema must define the behavior class and all its namespaced
    /// properties + kind/scope allowedTokens, so authored behavior content
    /// validates.
    #[test]
    fn generated_schema_defines_behavior_class_and_properties() {
        let text =
            std::fs::read_to_string(PathBuf::from(BAKED_PLUGIN_DIR).join("generatedSchema.usda"))
                .expect("read generatedSchema.usda");
        assert!(text.contains(&format!("class \"{BEHAVIOR_SCHEMA}\"")));
        for attr in [
            ATTR_BEHAVIOR_KIND,
            ATTR_BEHAVIOR_NAME,
            ATTR_BEHAVIOR_VERSION,
            ATTR_BEHAVIOR_SHIP,
            ATTR_BEHAVIOR_SOURCE,
            ATTR_BEHAVIOR_MODULE,
            ATTR_BEHAVIOR_UUID,
            ATTR_BEHAVIOR_ENABLED,
            ATTR_BEHAVIOR_SCOPE,
            ATTR_BEHAVIOR_SERVER_AUTHORITATIVE,
            ATTR_BEHAVIOR_CAPABILITIES,
            ATTR_BEHAVIOR_INPUTS,
            ATTR_BEHAVIOR_MEMORY_MB,
        ] {
            assert!(text.contains(attr), "generatedSchema missing {attr}");
        }
        for token in BEHAVIOR_KIND_TOKENS
            .iter()
            .chain(BEHAVIOR_SCOPE_TOKENS.iter())
            .chain(BEHAVIOR_CAPABILITY_TOKENS.iter())
        {
            assert!(
                text.contains(&format!("\"{token}\"")),
                "generatedSchema missing token {token}"
            );
        }
        // The retired sidecar manifest attribute must be GONE.
        assert!(
            !text.contains("idtx:behavior:manifest"),
            "retired idtx:behavior:manifest still present in generatedSchema"
        );
        // The retired free-floating camelCase spellings must be GONE.
        assert!(
            !text.contains("idtx:behaviorKind"),
            "retired idtx:behaviorKind still present in generatedSchema"
        );
    }

    #[test]
    fn widget_billboard_token_roundtrips_for_all_variants() {
        for mode in WidgetBillboard::ALL {
            assert_eq!(WidgetBillboard::from_token(mode.as_token()), Some(mode));
        }
        assert_eq!(WidgetBillboard::None.as_token(), "none");
        assert_eq!(WidgetBillboard::ScreenAligned.as_token(), "screenAligned");
        assert_eq!(WidgetBillboard::default(), WidgetBillboard::None);
        // camelCase spelling matters; wrong case / unknown is rejected.
        assert_eq!(WidgetBillboard::from_token("screenaligned"), None);
        assert_eq!(WidgetBillboard::from_token("None"), None);
        assert_eq!(WidgetBillboard::from_token(""), None);
    }

    #[test]
    fn widget_space_layout_alignment_fit_tokens_roundtrip() {
        for space in WidgetSpace::ALL {
            assert_eq!(WidgetSpace::from_token(space.as_token()), Some(space));
        }
        assert_eq!(WidgetSpace::default(), WidgetSpace::World);
        for layout in WidgetLayout::ALL {
            assert_eq!(WidgetLayout::from_token(layout.as_token()), Some(layout));
        }
        assert_eq!(WidgetLayout::default(), WidgetLayout::Column);
        for align in WidgetAlignment::ALL {
            assert_eq!(WidgetAlignment::from_token(align.as_token()), Some(align));
        }
        assert_eq!(WidgetAlignment::default(), WidgetAlignment::Start);
        for fit in WidgetFit::ALL {
            assert_eq!(WidgetFit::from_token(fit.as_token()), Some(fit));
        }
        assert_eq!(WidgetFit::default(), WidgetFit::Contain);
        // Unknown tokens rejected.
        assert_eq!(WidgetSpace::from_token("screen"), None);
        assert_eq!(WidgetLayout::from_token("flex"), None);
        assert_eq!(WidgetFit::from_token("stretch"), None);
    }

    #[test]
    fn widget_attribute_constants_are_namespaced_under_widget_ns() {
        for attr in [
            ATTR_WIDGET_SIZE,
            ATTR_WIDGET_RESOLUTION,
            ATTR_WIDGET_PIXELS_PER_UNIT,
            ATTR_WIDGET_BILLBOARD,
            ATTR_WIDGET_DOUBLE_SIDED,
            ATTR_WIDGET_OPACITY,
            ATTR_WIDGET_SPACE,
            ATTR_WIDGET_CONTENT_SOURCE,
            ATTR_WIDGET_LAYOUT,
            ATTR_WIDGET_GAP,
            ATTR_WIDGET_PADDING,
            ATTR_WIDGET_ALIGNMENT,
            ATTR_WIDGET_TEXT,
            ATTR_WIDGET_FONT_SIZE,
            ATTR_WIDGET_COLOR,
            ATTR_WIDGET_FONT,
            ATTR_WIDGET_FONT_FAMILY,
            ATTR_WIDGET_LABEL,
            ATTR_WIDGET_ENABLED,
            ATTR_WIDGET_IMAGE,
            ATTR_WIDGET_FIT,
            ATTR_WIDGET_VALUE,
            ATTR_WIDGET_MIN,
            ATTR_WIDGET_MAX,
        ] {
            assert!(
                attr.starts_with(&format!("{WIDGET_NS}:")),
                "{attr} must be under {WIDGET_NS}"
            );
        }
        assert_eq!(ATTR_WIDGET_SIZE, "idtx:widget:size");
        assert_eq!(ATTR_WIDGET_FONT, "idtx:widget:font");
        assert_eq!(ATTR_WIDGET_VALUE, "idtx:widget:value");
        // The panel type name must match the usdPhysics-style bare spelling the
        // bridge `prim_is_a` / `type_name` compare (no `Usd` prefix).
        assert_eq!(WIDGET_PANEL_SCHEMA, "IdtxWidgetPanel");
        assert_eq!(WIDGET_SCHEMAS.len(), 11);
        assert_eq!(WIDGET_SCHEMAS[0], WIDGET_PANEL_SCHEMA);
        for field in [
            WIDGET_FIELD_DOUBLE_SCHEMA,
            WIDGET_FIELD_FLOAT_SCHEMA,
            WIDGET_FIELD_INT_SCHEMA,
            WIDGET_FIELD_STRING_SCHEMA,
        ] {
            assert!(WIDGET_SCHEMAS.contains(&field), "{field} must be a populator type");
        }
        // The abstract field base is not instantiable, so it is NOT a populator type.
        assert!(!WIDGET_SCHEMAS.contains(&WIDGET_FIELD_SCHEMA));
    }

    /// `plugInfo.json` must declare `IdtxWidgetPanel` as a codeless concrete
    /// typed prim schema deriving from `UsdGeomGprim` (so `DefinePrim(path,
    /// "IdtxWidgetPanel")` resolves and the prim is a renderable gprim in 3D),
    /// with NO per-type `alias` block (double-registration gotcha — see the
    /// codeless schema memory note). The panel is the only widget prim that enters
    /// real 3D space; its element children are Imageable, not Gprim.
    #[test]
    fn plug_info_declares_codeless_gprim_widget_panel() {
        let dir = PathBuf::from(BAKED_PLUGIN_DIR);
        let json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("plugInfo.json")).expect("read plugInfo.json"),
        )
        .expect("plugInfo.json is valid JSON");
        let panel = &json["Plugins"][0]["Info"]["Types"][WIDGET_PANEL_SCHEMA];
        assert_eq!(panel["schemaKind"], "concreteTyped");
        assert_eq!(
            panel["bases"][0], "UsdGeomGprim",
            "the panel must derive from Gprim (a renderable geometric object in 3D)"
        );
        assert_eq!(panel["schemaIdentifier"], WIDGET_PANEL_SCHEMA);
        assert!(
            panel.get("alias").is_none(),
            "codeless schema must not declare a per-type alias in plugInfo"
        );
    }

    /// The abstract `IdtxWidgetElement` base must be declared `abstractTyped`
    /// (not instantiable) deriving from `UsdGeomImageable` — a pure visual with no
    /// transform/bounds — alias-free. Every element type derives from it.
    #[test]
    fn plug_info_declares_abstract_imageable_element_base() {
        let dir = PathBuf::from(BAKED_PLUGIN_DIR);
        let json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("plugInfo.json")).expect("read plugInfo.json"),
        )
        .expect("plugInfo.json is valid JSON");
        let el = &json["Plugins"][0]["Info"]["Types"][WIDGET_ELEMENT_SCHEMA];
        assert_eq!(el["schemaKind"], "abstractTyped", "element base must be abstract");
        assert_eq!(
            el["bases"][0], "UsdGeomImageable",
            "the element base must derive from Imageable (visual, no transform/bounds)"
        );
        assert!(el.get("alias").is_none());
    }

    /// Every widget element prim must be declared codeless concreteTyped deriving
    /// from the abstract `IdtxWidgetElement` base, alias-free.
    #[test]
    fn plug_info_declares_codeless_typed_widget_elements() {
        let dir = PathBuf::from(BAKED_PLUGIN_DIR);
        let json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("plugInfo.json")).expect("read plugInfo.json"),
        )
        .expect("plugInfo.json is valid JSON");
        for schema in [
            WIDGET_CONTAINER_SCHEMA,
            WIDGET_LABEL_SCHEMA,
            WIDGET_BUTTON_SCHEMA,
            WIDGET_IMAGE_SCHEMA,
            WIDGET_SLIDER_SCHEMA,
            WIDGET_TOGGLE_SCHEMA,
        ] {
            let ty = &json["Plugins"][0]["Info"]["Types"][schema];
            assert_eq!(ty["schemaKind"], "concreteTyped", "{schema} schemaKind");
            assert_eq!(ty["bases"][0], WIDGET_ELEMENT_SCHEMA, "{schema} base");
            assert_eq!(ty["schemaIdentifier"], schema, "{schema} identifier");
            assert!(
                ty.get("alias").is_none(),
                "{schema} codeless schema must not declare a per-type alias"
            );
        }
    }

    /// The abstract `IdtxWidgetField` base must be declared `abstractTyped`
    /// (deriving from `IdtxWidgetElement`, so it is a positioned element yet cannot
    /// be instantiated) and every concrete `IdtxWidgetField*` subclass
    /// `concreteTyped` deriving from it — all alias-free. This is the value-field
    /// split: a label stores text, a typed field carries the compute-bound
    /// `idtx:widget:value`.
    #[test]
    fn plug_info_declares_abstract_field_base_and_concrete_subclasses() {
        let dir = PathBuf::from(BAKED_PLUGIN_DIR);
        let json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("plugInfo.json")).expect("read plugInfo.json"),
        )
        .expect("plugInfo.json is valid JSON");
        let types = &json["Plugins"][0]["Info"]["Types"];

        let base = &types[WIDGET_FIELD_SCHEMA];
        assert_eq!(base["schemaKind"], "abstractTyped", "field base must be abstract");
        assert_eq!(base["bases"][0], WIDGET_ELEMENT_SCHEMA);
        assert!(base.get("alias").is_none());

        for field in [
            WIDGET_FIELD_DOUBLE_SCHEMA,
            WIDGET_FIELD_FLOAT_SCHEMA,
            WIDGET_FIELD_INT_SCHEMA,
            WIDGET_FIELD_STRING_SCHEMA,
        ] {
            let ty = &types[field];
            assert_eq!(ty["schemaKind"], "concreteTyped", "{field} schemaKind");
            assert_eq!(
                ty["bases"][0], WIDGET_FIELD_SCHEMA,
                "{field} must derive from the abstract field base"
            );
            assert!(ty.get("alias").is_none(), "{field} must be alias-free");
        }
    }

    /// The generated schema must define every widget class and its namespaced
    /// properties + allowedTokens, so authored widget content validates and the
    /// panel's schema fallbacks (e.g. `size = (0.2, 0.2)`) resolve.
    #[test]
    fn generated_schema_defines_widget_classes_and_properties() {
        let text =
            std::fs::read_to_string(PathBuf::from(BAKED_PLUGIN_DIR).join("generatedSchema.usda"))
                .expect("read generatedSchema.usda");
        for schema in WIDGET_SCHEMAS {
            assert!(
                text.contains(&format!("class {schema} \"{schema}\"")),
                "generatedSchema missing class {schema}"
            );
        }
        for attr in [
            ATTR_WIDGET_SIZE,
            ATTR_WIDGET_BILLBOARD,
            ATTR_WIDGET_SPACE,
            ATTR_WIDGET_POSITION,
            ATTR_WIDGET_ORDER,
            ATTR_WIDGET_LAYOUT,
            ATTR_WIDGET_TEXT,
            ATTR_WIDGET_FONT,
            ATTR_WIDGET_FIT,
            ATTR_WIDGET_VALUE,
        ] {
            assert!(text.contains(attr), "generatedSchema missing {attr}");
        }
        for token in WidgetBillboard::ALL
            .iter()
            .map(|m| m.as_token())
            .chain(WidgetSpace::ALL.iter().map(|s| s.as_token()))
            .chain(WidgetLayout::ALL.iter().map(|l| l.as_token()))
            .chain(WidgetFit::ALL.iter().map(|f| f.as_token()))
        {
            assert!(
                text.contains(&format!("\"{token}\"")),
                "generatedSchema missing widget token {token}"
            );
        }
    }
}
