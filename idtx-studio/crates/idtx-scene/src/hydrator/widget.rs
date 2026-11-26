
use bevy_ecs::prelude::*;
use idtx_usd::{PrimId, StageService};
use crate::widget_layout::{RadialSettings, WidgetPolygon, WidgetSurface};

use crate::{
    components::{HudComp, PrimNode, WidgetElement, WidgetElementKind, WidgetPanelComp},
    hydrator::{
        read_asset_attribute, read_bool_attribute, read_color3f_attribute, read_double_attribute,
        read_float2_attribute, read_float_attribute, read_int2_attribute, read_int_attribute,
        read_string_attribute, read_token_attribute, SceneHydrator,
    },
    schema_ext::{SchemaHydrationCtx, SchemaHydrator},
};

/// Built-in [`SchemaHydrator`] for the `IdtxHud` (`idtx:hud:*`) concrete-typed
/// `IsA` schema, projecting [`HudComp`].
#[derive(Debug)]
pub struct HudSchemaHydrator;

/// Built-in [`SchemaHydrator`] for the `IdtxWidgetPanel` (`idtx:widget:*`)
/// concrete-typed `IsA` schema (and its element descendants), projecting
/// [`WidgetPanelComp`].
#[derive(Debug)]
pub struct WidgetSchemaHydrator;

impl SchemaHydrator for HudSchemaHydrator {
    fn name(&self) -> &'static str {
        "hud"
    }

    /// Hydrate [`HudComp`] from the `IdtxHud` (`idtx:hud:*`) concrete-typed
    /// `IsA` schema.
    ///
    /// Scans every entity; prims whose `type_name` is `"IdtxHud"` get a fresh
    /// [`HudComp`] (or a no-op update when nothing changed). Prims of any other
    /// type have any stale [`HudComp`] removed, so the scan doubles as the
    /// reconcile path. Returns the number of components added, refreshed, or
    /// removed.
    fn hydrate(&self, ctx: &mut SchemaHydrationCtx<'_>, service: &StageService) -> usize {
        use idtx_usd_extension::{
            HudAnchor, ATTR_HUD_ANCHOR, ATTR_HUD_IMAGE, ATTR_HUD_OPACITY, ATTR_HUD_SIZE_PX,
            HUD_SCHEMA,
        };

        let mut changed = 0;
        for (entity, prim) in ctx.entity_prims() {
            let type_name = ctx
                .world
                .get::<PrimNode>(entity)
                .map(|n| n.type_name.clone())
                .unwrap_or_default();

            if type_name != HUD_SCHEMA {
                if ctx.world.get::<HudComp>(entity).is_some() {
                    ctx.world.entity_mut(entity).remove::<HudComp>();
                    changed += 1;
                }
                continue;
            }

            // Resolve the image through the USD asset resolver (search paths +
            // layer anchoring), matching how material/dome textures resolve —
            // the render texture cache opens an already-resolved path and does
            // NOT run search-path resolution itself. Fall back to the raw
            // authored path when the resolver yields nothing.
            let image = service
                .resolve_prim_asset(prim, ATTR_HUD_IMAGE)
                .ok()
                .flatten()
                .or_else(|| read_asset_attribute(service, prim, ATTR_HUD_IMAGE))
                .unwrap_or_default();
            let size_px = read_int2_attribute(service, prim, ATTR_HUD_SIZE_PX).unwrap_or([0, 0]);
            let opacity = read_float_attribute(service, prim, ATTR_HUD_OPACITY).unwrap_or(1.0);
            let anchor = read_token_attribute(service, prim, ATTR_HUD_ANCHOR)
                .and_then(|t| HudAnchor::from_token(&t))
                .unwrap_or_default();

            let next = HudComp { image, size_px, anchor, opacity };
            if ctx.world.get::<HudComp>(entity) != Some(&next) {
                ctx.world.entity_mut(entity).insert(next);
                changed += 1;
            }
        }
        changed
    }
}

impl SchemaHydrator for WidgetSchemaHydrator {
    fn name(&self) -> &'static str {
        "widget"
    }

    /// Hydrate [`WidgetPanelComp`] from the `IdtxWidgetPanel` (`idtx:widget:*`)
    /// concrete-typed `IsA` schema.
    ///
    /// Scans every entity; prims whose `type_name` is `"IdtxWidgetPanel"` get a
    /// fresh [`WidgetPanelComp`] (or a no-op update when nothing changed). For
    /// each panel the panel's element descendants (`IdtxWidget*` child prims)
    /// are walked **here, at drain time** — not per frame — and flattened into
    /// [`WidgetPanelComp::elements`]. Prims of any other type have any stale
    /// [`WidgetPanelComp`] removed, so the scan doubles as the reconcile path.
    ///
    /// Returns the number of components added, refreshed, or removed.
    fn hydrate(&self, ctx: &mut SchemaHydrationCtx<'_>, service: &StageService) -> usize {
        use idtx_usd_extension::{
            WidgetBillboard, WidgetLayout, WidgetSpace, ATTR_WIDGET_BILLBOARD, ATTR_WIDGET_COLOR,
            ATTR_WIDGET_CONTENT_SOURCE, ATTR_WIDGET_DOUBLE_SIDED, ATTR_WIDGET_ENABLED,
            ATTR_WIDGET_FONT_FAMILY, ATTR_WIDGET_FONT_SIZE, ATTR_WIDGET_IMAGE, ATTR_WIDGET_LABEL,
            ATTR_WIDGET_LAYOUT, ATTR_WIDGET_OPACITY, ATTR_WIDGET_ORDER, ATTR_WIDGET_PIXELS_PER_UNIT,
            ATTR_WIDGET_POSITION, ATTR_WIDGET_PRECISION, ATTR_WIDGET_RESOLUTION, ATTR_WIDGET_SIZE,
            ATTR_WIDGET_SPACE, ATTR_WIDGET_TEXT, ATTR_WIDGET_VALUE, WIDGET_CONTAINER_SCHEMA,
            WIDGET_PANEL_SCHEMA,
        };

        // One pass over all entities to snapshot (entity, prim, path, type) so
        // the per-panel element walk below does not re-borrow the entity index.
        let all: Vec<(Entity, PrimId, String, String)> = ctx
            .entity_prims()
            .into_iter()
            .map(|(entity, prim)| {
                let node = ctx.world.get::<PrimNode>(entity);
                let path = node.map_or_else(String::new, |n| n.path.clone());
                let type_name = node.map_or_else(String::new, |n| n.type_name.clone());
                (entity, prim, path, type_name)
            })
            .collect();

        let mut changed = 0;
        for (entity, prim, path, type_name) in &all {
            if type_name != WIDGET_PANEL_SCHEMA {
                if ctx.world.get::<WidgetPanelComp>(*entity).is_some() {
                    ctx.world.entity_mut(*entity).remove::<WidgetPanelComp>();
                    changed += 1;
                }
                continue;
            }

            let size = read_float2_attribute(service, *prim, ATTR_WIDGET_SIZE).unwrap_or([0.2, 0.2]);
            // Clamp stage-authored resolution to a documented per-axis maximum.
            // The value is untrusted (it comes straight from the stage): the
            // rasterizer allocates `w * h * 4` bytes and uploads a texture of that
            // size, so a hostile/mis-authored dimension near `i32::MAX` would
            // overflow the byte count or exhaust process/GPU memory. `clamp` keeps
            // each axis in `[1, MAX_WIDGET_AXIS]`, bounding one panel texture at
            // `4096 * 4096 * 4` = 64 MiB.
            const MAX_WIDGET_AXIS: i32 = 4096;
            let authored_resolution = read_int2_attribute(service, *prim, ATTR_WIDGET_RESOLUTION)
                .map_or([512, 512], |[w, h]| {
                    [
                        w.clamp(1, MAX_WIDGET_AXIS).cast_unsigned(),
                        h.clamp(1, MAX_WIDGET_AXIS).cast_unsigned(),
                    ]
                });
            let pixels_per_unit =
                read_float_attribute(service, *prim, ATTR_WIDGET_PIXELS_PER_UNIT).unwrap_or(2048.0);
            // #2 (readability lever, env-gated): derive the rasterization
            // resolution from the panel's PHYSICAL size × `pixels_per_unit` so a
            // large panel gets proportionally more texels regardless of the
            // hand-authored `idtx:widget:resolution`. Takes the per-axis max of
            // authored vs. derived (never fewer texels than authored), clamped to
            // the same untrusted-input ceiling. Opt-in until the readability
            // levers are chosen; see docs/widget-text-readability.md.
            let resolution = if std::env::var("IDTX_WIDGET_PPU_RES").is_ok() {
                let derived = |extent: f32| {
                    (extent * pixels_per_unit).round().clamp(1.0, MAX_WIDGET_AXIS as f32) as u32
                };
                [
                    authored_resolution[0].max(derived(size[0])),
                    authored_resolution[1].max(derived(size[1])),
                ]
            } else {
                authored_resolution
            };
            let billboard = read_token_attribute(service, *prim, ATTR_WIDGET_BILLBOARD)
                .and_then(|t| WidgetBillboard::from_token(&t))
                .unwrap_or_default();
            let double_sided =
                read_bool_attribute(service, *prim, ATTR_WIDGET_DOUBLE_SIDED).unwrap_or(true);
            let opacity = all.iter().filter(|(_, _, ancestor, _)| ancestor == path
                || path.strip_prefix(ancestor.as_str()).is_some_and(|tail| tail.starts_with('/')))
                .fold(1.0, |value, (_, ancestor, _, _)| value * opacity_value(
                    read_float_attribute(service, *ancestor, ATTR_WIDGET_OPACITY).unwrap_or(1.0)));
            let space = read_token_attribute(service, *prim, ATTR_WIDGET_SPACE)
                .and_then(|t| WidgetSpace::from_token(&t))
                .unwrap_or_default();
            let content_source = read_asset_attribute(service, *prim, ATTR_WIDGET_CONTENT_SOURCE);
            let surface = read_surface(service, *prim);

            // Walk this panel's element descendants (namespace order ≈ sorted
            // path order for the flat fixtures). Resolve each element's content.
            let prefix = format!("{path}/");
            let mut elements: Vec<(String, WidgetElement)> = Vec::new();
            for (el_entity, el_prim, el_path, el_type) in &all {
                if el_entity == entity || !el_path.starts_with(&prefix) {
                    continue;
                }
                // A nested panel owns its own elements. Visibility and opacity
                // are inherited through every ancestor, including non-widgets.
                let ancestors: Vec<_> = all.iter().filter(|(_, _, p, _)|
                    p.starts_with(&prefix) && (p == el_path || el_path.strip_prefix(p.as_str()).is_some_and(|s| s.starts_with('/')))
                ).collect();
                if ancestors.iter().any(|(e, _, p, t)|
                    (p != el_path && t == WIDGET_PANEL_SCHEMA)
                    || ctx.world.get::<crate::VisibilityComp>(*e).is_some_and(|v| !v.visible)) {
                    continue;
                }
                let element_opacity = ancestors.iter().fold(1.0, |a, (_, p, _, _)|
                    a * opacity_value(read_float_attribute(service, *p, ATTR_WIDGET_OPACITY).unwrap_or(1.0)));
                if element_opacity <= 0.0 { continue; }
                let Some(kind) = widget_element_kind(el_type) else {
                    continue;
                };
                let depth = u32::try_from(el_path[prefix.len()..].matches('/').count()).unwrap_or(0);
                // Layout of this element's immediate parent container (Column when
                // the parent is the panel). Lets a `row` container render its
                // children as columns (label | field).
                let parent_path = &el_path[..el_path.rfind('/').unwrap_or(0)];
                let parent_layout = all
                    .iter()
                    .find(|(_, _, p, t)| p == parent_path && t == WIDGET_CONTAINER_SCHEMA)
                    .and_then(|(_, pp, _, _)| read_token_attribute(service, *pp, ATTR_WIDGET_LAYOUT))
                    .and_then(|t| WidgetLayout::from_token(&t))
                    .unwrap_or_default();
                let (text, color, value) = read_widget_element_fields(
                    service, *el_prim, kind, el_type, ATTR_WIDGET_TEXT, ATTR_WIDGET_LABEL,
                    ATTR_WIDGET_COLOR, ATTR_WIDGET_VALUE, ATTR_WIDGET_ENABLED, ATTR_WIDGET_PRECISION,
                );
                // Shared `IdtxWidgetElement` layout attrs: a NORMALIZED position
                // ((0,0) default = "unpositioned" → namespace-order fallback) and a
                // paint/stacking order.
                let position = read_float2_attribute(service, *el_prim, ATTR_WIDGET_POSITION)
                    .filter(|p| *p != [0.0, 0.0]);
                let order = read_int_attribute(service, *el_prim, ATTR_WIDGET_ORDER).unwrap_or(0);
                // Per-element font configuration.
                // `fontFamily` selects a registered family (unknown/absent → the
                // built-in default in the renderer's font registry); `fontSize` is
                // in pixels of the panel's rasterization space, `None`/≤0 → auto-
                // proportional sizing.
                let font_family = read_token_attribute(service, *el_prim, ATTR_WIDGET_FONT_FAMILY)
                    .filter(|s| !s.is_empty());
                let font_size = read_float_attribute(service, *el_prim, ATTR_WIDGET_FONT_SIZE)
                    .filter(|s| *s > 0.0)
                    // `fontSize` is absolute pixels in rasterization space, so if
                    // #2 (ppu) raised the effective resolution above the authored
                    // one, scale authored sizes by the same factor to preserve
                    // apparent text size (auto-sized text is already slot-relative).
                    .map(|s| s * resolution[1] as f32 / authored_resolution[1].max(1) as f32);
                // Panel-relative namespace path (the stable identity an
                // interaction handler addresses, e.g. "Footer/QuitButton").
                let rel_path = el_path[prefix.len()..].to_string();
                // `enabled` only drives a button's interaction/dimming; default
                // true for every kind when the attribute is absent.
                let enabled =
                    read_bool_attribute(service, *el_prim, ATTR_WIDGET_ENABLED).unwrap_or(true);
                // `IdtxWidgetImage` only: resolve the image asset path through the
                // USD resolver (search paths + layer anchoring), matching how
                // material and dome textures resolve. Fall back to the raw authored
                // path when the resolver yields nothing; `None` for every other kind.
                let image_path = if kind == WidgetElementKind::Image {
                    service
                        .resolve_prim_asset(*el_prim, ATTR_WIDGET_IMAGE)
                        .ok()
                        .flatten()
                        .or_else(|| read_asset_attribute(service, *el_prim, ATTR_WIDGET_IMAGE))
                } else {
                    None
                };
                elements.push((
                    el_path.clone(),
                    WidgetElement {
                        kind,
                        path: rel_path,
                        depth,
                        parent_layout,
                        layout: read_token_attribute(service, *el_prim, ATTR_WIDGET_LAYOUT)
                            .and_then(|s| WidgetLayout::from_token(&s)).unwrap_or_default(),
                        radial: read_radial(service, *el_prim),
                        opacity: element_opacity,
                        text,
                        color,
                        value,
                        position,
                        order,
                        font_family,
                        font_size,
                        enabled,
                        image_path,
                    },
                ));
            }
            // Namespace order within an `order` group (default order = 0 → pure
            // namespace order, the pre-position behavior); a higher `order` sorts
            // later so it paints on top.
            elements.sort_by(|a, b| a.1.order.cmp(&b.1.order).then_with(|| a.0.cmp(&b.0)));
            let elements: Vec<WidgetElement> = elements.into_iter().map(|(_, e)| e).collect();

            let content_hash = widget_content_hash(
                size, resolution, pixels_per_unit, billboard, double_sided, opacity, space,
                content_source.as_deref(), &elements, &surface,
            );

            let next = WidgetPanelComp {
                size,
                resolution,
                pixels_per_unit,
                billboard,
                double_sided,
                opacity,
                space,
                content_source,
                surface,
                elements,
                content_hash,
            };
            if ctx.world.get::<WidgetPanelComp>(*entity) != Some(&next) {
                ctx.world.entity_mut(*entity).insert(next);
                changed += 1;
            }
        }
        changed
    }
}

// ── Thin inherent wrappers (preserve the existing public method surface) ────────

impl SceneHydrator {
    /// Delegates to [`HudSchemaHydrator`].
    pub fn hydrate_hud(&mut self, service: &StageService) -> usize {
        let mut ctx = self.schema_ctx();
        HudSchemaHydrator.hydrate(&mut ctx, service)
    }

    /// Delegates to [`WidgetSchemaHydrator`].
    pub fn hydrate_widgets(&mut self, service: &StageService) -> usize {
        let mut ctx = self.schema_ctx();
        WidgetSchemaHydrator.hydrate(&mut ctx, service)
    }
}

/// Maps an `IdtxWidget*` element prim type name to its [`WidgetElementKind`].
/// Returns `None` for the panel itself or any non-widget type.
fn widget_element_kind(type_name: &str) -> Option<WidgetElementKind> {
    use idtx_usd_extension::{
        WIDGET_BUTTON_SCHEMA, WIDGET_CONTAINER_SCHEMA, WIDGET_FIELD_DOUBLE_SCHEMA,
        WIDGET_FIELD_FLOAT_SCHEMA, WIDGET_FIELD_INT_SCHEMA, WIDGET_FIELD_STRING_SCHEMA,
        WIDGET_IMAGE_SCHEMA, WIDGET_LABEL_SCHEMA, WIDGET_SLIDER_SCHEMA, WIDGET_TOGGLE_SCHEMA,
    };
    match type_name {
        t if t == WIDGET_CONTAINER_SCHEMA => Some(WidgetElementKind::Container),
        t if t == WIDGET_LABEL_SCHEMA => Some(WidgetElementKind::Label),
        t if t == WIDGET_BUTTON_SCHEMA => Some(WidgetElementKind::Button),
        t if t == WIDGET_IMAGE_SCHEMA => Some(WidgetElementKind::Image),
        t if t == WIDGET_SLIDER_SCHEMA => Some(WidgetElementKind::Slider),
        t if t == WIDGET_TOGGLE_SCHEMA => Some(WidgetElementKind::Toggle),
        t if t == WIDGET_FIELD_DOUBLE_SCHEMA
            || t == WIDGET_FIELD_FLOAT_SCHEMA
            || t == WIDGET_FIELD_INT_SCHEMA
            || t == WIDGET_FIELD_STRING_SCHEMA =>
        {
            Some(WidgetElementKind::Field)
        }
        _ => None,
    }
}

/// Resolve the `(text, color, value)` of one widget element from its authored
/// attributes, keyed by kind. A Label reads `idtx:widget:text` + `:color` (text
/// only — no bound value). A `Field` (`IdtxWidgetField*`) reads its typed
/// `idtx:widget:value` and resolves the *formatted* display string — the value
/// type is selected by `type_name` (double/float formatted at `:precision`, int
/// and string shown verbatim). A Button reads `:label`; a Slider/Toggle reads
/// `:value`.
#[allow(clippy::too_many_arguments)]
fn read_widget_element_fields(
    service: &StageService,
    prim: PrimId,
    kind: WidgetElementKind,
    type_name: &str,
    attr_text: &str,
    attr_label: &str,
    attr_color: &str,
    attr_value: &str,
    attr_enabled: &str,
    attr_precision: &str,
) -> (String, [f32; 3], f32) {
    use idtx_usd_extension::{WIDGET_FIELD_INT_SCHEMA, WIDGET_FIELD_STRING_SCHEMA};
    // A fixed accent, since there is no IdtxWidgetStyleAPI, so buttons/sliders
    // are visually distinct from labels in the solid-fill rasterizer.
    const ACCENT: [f32; 3] = [0.25, 0.5, 0.9];
    match kind {
        WidgetElementKind::Label => {
            let text = read_string_attribute(service, prim, attr_text).unwrap_or_default();
            let color = read_color3f_attribute(service, prim, attr_color).unwrap_or([1.0, 1.0, 1.0]);
            (text, color, 0.0)
        }
        WidgetElementKind::Field => {
            // Bindable value readout. `idtx:widget:value` is the compute leaf whose
            // concrete type is the field subclass (double/float/int/string). It is
            // formatted host-side: float/double at the authored `:precision`, int
            // and string verbatim. An openExec writeback re-authors `value`, the
            // value-only `widget_changed` trigger re-hydrates, and `content_hash`
            // (which includes `text`) re-rasterizes.
            let color = read_color3f_attribute(service, prim, attr_color).unwrap_or([1.0, 1.0, 1.0]);
            let (text, value) = if type_name == WIDGET_FIELD_STRING_SCHEMA {
                (read_string_attribute(service, prim, attr_value).unwrap_or_default(), 0.0)
            } else if type_name == WIDGET_FIELD_INT_SCHEMA {
                // `value` (the f32 slot) is unused for a field's display (the
                // formatted `text` is); keep it 0 rather than lossily cast the int.
                let v = read_int_attribute(service, prim, attr_value).unwrap_or(0);
                (v.to_string(), 0.0)
            } else {
                // Double / float: format at the authored precision.
                let v = read_double_attribute(service, prim, attr_value).unwrap_or(0.0);
                // USD integers are untrusted. Rust formatting panics for large
                // dynamic precisions, and smaller excessive values still create
                // huge display strings. Bound the value before invoking format!.
                // Preserve the existing default for absent/negative precision.
                const MAX_FIELD_PRECISION: usize = 16;
                let authored = read_int_attribute(service, prim, attr_precision).unwrap_or(3);
                let requested = usize::try_from(authored).ok();
                let precision = requested.unwrap_or(3).min(MAX_FIELD_PRECISION);
                if requested.is_none_or(|p| p > MAX_FIELD_PRECISION) {
                    tracing::warn!(
                        path = service.path_for(prim).unwrap_or("<unknown>"),
                        authored, precision, "widget field precision is outside 0..=16; using bounded display precision"
                    );
                }
                (format!("{v:.precision$}"), v)
            };
            (text, color, value)
        }
        WidgetElementKind::Button => {
            let text = read_string_attribute(service, prim, attr_label).unwrap_or_default();
            let enabled = read_bool_attribute(service, prim, attr_enabled).unwrap_or(true);
            let color = read_color3f_attribute(service, prim, attr_color)
                .unwrap_or(if enabled { ACCENT } else { [0.4, 0.4, 0.45] });
            (text, color, 0.0)
        }
        WidgetElementKind::Slider => {
            let value = read_float_attribute(service, prim, attr_value).unwrap_or(0.0);
            (String::new(), ACCENT, value)
        }
        WidgetElementKind::Toggle => {
            let on = read_bool_attribute(service, prim, attr_value).unwrap_or(false);
            (String::new(), ACCENT, if on { 1.0 } else { 0.0 })
        }
        WidgetElementKind::Image => {
            let color = read_color3f_attribute(service, prim, attr_color)
                .unwrap_or([0.3, 0.3, 0.35]);
            (String::new(), color, 0.0)
        }
        WidgetElementKind::Container => {
            (String::new(), [0.3, 0.3, 0.35], 0.0)
        }
    }
}

/// Stable hash of every rasterization input of a panel — the dirty-gating key.
/// Floats are hashed by bit pattern (`to_bits`) so the result is deterministic.
#[allow(clippy::too_many_arguments)]
fn widget_content_hash(
    size: [f32; 2],
    resolution: [u32; 2],
    pixels_per_unit: f32,
    billboard: idtx_usd_extension::WidgetBillboard,
    double_sided: bool,
    opacity: f32,
    space: idtx_usd_extension::WidgetSpace,
    content_source: Option<&str>,
    elements: &[WidgetElement],
    surface: &WidgetSurface,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    size[0].to_bits().hash(&mut h);
    size[1].to_bits().hash(&mut h);
    resolution.hash(&mut h);
    pixels_per_unit.to_bits().hash(&mut h);
    billboard.as_token().hash(&mut h);
    double_sided.hash(&mut h);
    opacity.to_bits().hash(&mut h);
    space.as_token().hash(&mut h);
    content_source.hash(&mut h);
    surface.opacity.to_bits().hash(&mut h);
    surface.color.map(f32::to_bits).hash(&mut h);
    surface.outline.is_some().hash(&mut h);
    if let Some(outline) = &surface.outline {
        for v in outline.vertices() { v.map(f32::to_bits).hash(&mut h); }
        outline.triangles().hash(&mut h);
    }
    for el in elements {
        (el.kind as u8).hash(&mut h);
        el.path.hash(&mut h);
        el.depth.hash(&mut h);
        el.parent_layout.as_token().hash(&mut h);
        el.layout.as_token().hash(&mut h);
        el.opacity.to_bits().hash(&mut h);
        el.radial.center.map(f32::to_bits).hash(&mut h);
        for v in [el.radial.inner_radius, el.radial.outer_radius, el.radial.start_angle,
            el.radial.sweep_angle, el.radial.gap_angle] { v.to_bits().hash(&mut h); }
        el.text.hash(&mut h);
        el.color[0].to_bits().hash(&mut h);
        el.color[1].to_bits().hash(&mut h);
        el.color[2].to_bits().hash(&mut h);
        el.value.to_bits().hash(&mut h);
        el.enabled.hash(&mut h);
        match el.position {
            Some([x, y]) => {
                1u8.hash(&mut h);
                x.to_bits().hash(&mut h);
                y.to_bits().hash(&mut h);
            }
            None => 0u8.hash(&mut h),
        }
        el.order.hash(&mut h);
        // Font configuration: a family or size edit must re-rasterize.
        el.font_family.hash(&mut h);
        match el.font_size {
            Some(s) => {
                1u8.hash(&mut h);
                s.to_bits().hash(&mut h);
            }
            None => 0u8.hash(&mut h),
        }
        // Image path: a new/changed image must re-rasterize.
        el.image_path.hash(&mut h);
    }
    h.finish()
}

fn opacity_value(value: f32) -> f32 {
    if value.is_finite() { value.clamp(0.0, 1.0) } else { 0.0 }
}

fn read_radial(service: &StageService, prim: PrimId) -> RadialSettings {
    use idtx_usd_extension::*;
    let d = RadialSettings::default();
    let f = |attr, default| read_float_attribute(service, prim, attr).unwrap_or(default);
    RadialSettings {
        center: read_float2_attribute(service, prim, ATTR_WIDGET_RADIAL_CENTER).unwrap_or(d.center),
        inner_radius: f(ATTR_WIDGET_RADIAL_INNER_RADIUS, d.inner_radius),
        outer_radius: f(ATTR_WIDGET_RADIAL_OUTER_RADIUS, d.outer_radius),
        start_angle: f(ATTR_WIDGET_RADIAL_START_ANGLE, d.start_angle),
        sweep_angle: f(ATTR_WIDGET_RADIAL_SWEEP_ANGLE, d.sweep_angle),
        gap_angle: f(ATTR_WIDGET_RADIAL_GAP_ANGLE, d.gap_angle),
        dead_zone: f(ATTR_WIDGET_RADIAL_DEAD_ZONE, d.dead_zone),
        hysteresis_angle: f(ATTR_WIDGET_RADIAL_HYSTERESIS, d.hysteresis_angle),
    }
}

fn read_surface(service: &StageService, prim: PrimId) -> WidgetSurface {
    use idtx_usd_extension::*;
    let defaults = WidgetSurface::default();
    let vertices = service.eval_float2_array(prim, ATTR_WIDGET_BACKGROUND_VERTICES, 0.0, true);
    let indices = service.eval_int_array(prim, ATTR_WIDGET_BACKGROUND_TRIANGLES, 0.0, true);
    let outline = match (vertices, indices) {
        (Ok(v), Ok(i)) if v.is_empty() && i.is_empty() => None,
        (Ok(v), Ok(i)) => {
            let mesh = i.into_iter().map(u32::try_from).collect::<Result<Vec<_>, _>>()
                .map_err(|_| "negative widget background index")
                .and_then(|i| WidgetPolygon::new(v, i));
            if let Err(message) = &mesh { tracing::warn!(?prim, message, "invalid widget outline"); }
            Some(mesh.unwrap_or_default())
        }
        _ => Some(WidgetPolygon::default()),
    };
    WidgetSurface { outline,
        color: read_color3f_attribute(service, prim, ATTR_WIDGET_BACKGROUND_COLOR).unwrap_or(defaults.color),
        opacity: opacity_value(read_float_attribute(service, prim, ATTR_WIDGET_BACKGROUND_OPACITY).unwrap_or(defaults.opacity)),
    }
}
