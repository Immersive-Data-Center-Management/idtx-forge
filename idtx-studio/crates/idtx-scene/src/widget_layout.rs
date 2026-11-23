//! Backend-neutral widget geometry. Pixel centers, paint order and clipping are
//! shared by the CPU rasterizer, preview, ray picking and direct navigation.

use crate::{WidgetBillboard, WidgetElement, WidgetElementKind, WidgetLayout};
use glam::{Mat4, Vec3};

/// Central interaction camera in render/stage coordinates (one basis per frame).
#[derive(Debug, Clone, Copy, bevy_ecs::prelude::Resource)]
pub struct WidgetInteractionCamera(pub Mat4);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RadialSettings {
    pub center: [f32; 2],
    pub inner_radius: f32,
    pub outer_radius: f32,
    pub start_angle: f32,
    pub sweep_angle: f32,
    /// Angular gap at the inner ring, converted once to constant pixel spacing.
    pub gap_angle: f32,
    pub dead_zone: f32,
    pub hysteresis_angle: f32,
}

impl Default for RadialSettings {
    fn default() -> Self {
        Self {
            center: [0.5, 0.5],
            inner_radius: 0.15,
            outer_radius: 0.5,
            start_angle: 0.0,
            sweep_angle: 360.0,
            gap_angle: 2.0,
            dead_zone: 0.25,
            hysteresis_angle: 4.0,
        }
    }
}

impl RadialSettings {
    pub fn valid(self) -> bool {
        self.center
            .into_iter()
            .all(|v| v.is_finite() && (0.0..=1.0).contains(&v))
            && self.inner_radius.is_finite()
            && self.inner_radius >= 0.0
            && self.outer_radius.is_finite()
            && self.outer_radius > self.inner_radius
            && self.outer_radius <= 0.5
            && self.start_angle.is_finite()
            && self.sweep_angle.is_finite()
            && self.sweep_angle > 0.0
            && self.sweep_angle <= 360.0
            && self.gap_angle.is_finite()
            && self.gap_angle >= 0.0
            && self.dead_zone.is_finite()
            && (0.0..1.0).contains(&self.dead_zone)
            && self.hysteresis_angle.is_finite()
            && (0.0..=45.0).contains(&self.hysteresis_angle)
    }
}

/// A validated, normalized triangle mesh. Private topology prevents bypassing
/// validation. An empty mesh is an empty surface (distinct from no outline).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WidgetPolygon {
    vertices: Vec<[f32; 2]>,
    triangles: Vec<[u32; 3]>,
}

impl WidgetPolygon {
    pub fn new(vertices: Vec<[f32; 2]>, indices: Vec<u32>) -> Result<Self, &'static str> {
        if vertices.len() > 4096
            || indices.len() > 8192 * 3
            || indices.len() % 3 != 0
            || vertices.is_empty()
            || indices.is_empty()
            || vertices
                .iter()
                .flatten()
                .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
            || indices.iter().any(|&i| i as usize >= vertices.len())
        {
            return Err("invalid widget background topology");
        }
        let triangles: Vec<_> = indices
            .chunks_exact(3)
            .map(|t| [t[0], t[1], t[2]])
            .collect();
        if triangles.iter().any(|t| {
            edge(
                vertices[t[0] as usize],
                vertices[t[1] as usize],
                vertices[t[2] as usize],
            )
            .abs()
                <= 1e-12
        }) {
            return Err("degenerate widget background triangle");
        }
        Ok(Self {
            vertices,
            triangles,
        })
    }

    pub fn vertices(&self) -> &[[f32; 2]] {
        &self.vertices
    }
    pub fn triangles(&self) -> &[[u32; 3]] {
        &self.triangles
    }

    pub fn contains(&self, p: [f32; 2]) -> bool {
        self.triangles.iter().any(|t| {
            let [a, b, c] = t.map(|i| self.vertices[i as usize]);
            let e = [edge(a, b, p), edge(b, c, p), edge(c, a, p)];
            e.iter().all(|&x| x >= 0.0) || e.iter().all(|&x| x <= 0.0)
        })
    }
}

fn edge(a: [f32; 2], b: [f32; 2], p: [f32; 2]) -> f32 {
    (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
}

#[derive(Debug, Clone, PartialEq)]
pub struct WidgetSurface {
    pub outline: Option<WidgetPolygon>,
    pub color: [f32; 3],
    pub opacity: f32,
}

impl Default for WidgetSurface {
    fn default() -> Self {
        Self {
            outline: None,
            color: [18.0 / 255.0, 20.0 / 255.0, 28.0 / 255.0],
            opacity: 235.0 / 255.0,
        }
    }
}

impl WidgetSurface {
    pub fn contains(&self, uv: [f32; 2]) -> bool {
        uv.into_iter().all(|v| (0.0..1.0).contains(&v))
            && self.outline.as_ref().is_none_or(|p| p.contains(uv))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WidgetElementRect {
    pub index: usize,
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
}

impl WidgetElementRect {
    pub fn contains(self, p: [f32; 2]) -> bool {
        p[0] >= self.x0 as f32
            && p[0] < self.x1 as f32
            && p[1] >= self.y0 as f32
            && p[1] < self.y1 as f32
    }
    pub fn center(self) -> [f32; 2] {
        [
            (self.x0 + self.x1) as f32 * 0.5,
            (self.y0 + self.y1) as f32 * 0.5,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WidgetShape {
    Rect(WidgetElementRect),
    AnnularSector {
        center: [f32; 2],
        inner: f32,
        outer: f32,
        start: f32,
        sweep: f32,
        /// Per-edge perpendicular inset in pixels. Adjacent sectors leave a
        /// parallel-sided gap of twice this width rather than an angular wedge.
        gap_half_width: f32,
    },
}

impl WidgetShape {
    /// Angular coverage at a given radius after the constant-width edge inset.
    /// `asin(inset / radius)` keeps the side a fixed distance from the slot's
    /// radial boundary, so the angular gap narrows toward the outer ring.
    pub fn angular_span(self, radius: f32) -> Option<(f32, f32)> {
        let Self::AnnularSector {
            start,
            sweep,
            gap_half_width,
            ..
        } = self
        else {
            return None;
        };
        if !radius.is_finite() || radius <= 0.0 || gap_half_width > radius {
            return None;
        }
        let inset = (gap_half_width / radius).asin().to_degrees();
        let sweep = sweep - 2.0 * inset;
        (sweep > 0.0).then_some((start + inset, sweep))
    }

    pub fn contains(self, p: [f32; 2]) -> bool {
        match self {
            Self::Rect(r) => r.contains(p),
            Self::AnnularSector {
                center,
                inner,
                outer,
                ..
            } => {
                let x = p[0] - center[0];
                let y = p[1] - center[1];
                let r2 = x * x + y * y;
                r2 > 0.0
                    && r2 >= inner * inner
                    && r2 < outer * outer
                    && self.angular_span(r2.sqrt()).is_some_and(|(start, sweep)| {
                        (x.atan2(-y).to_degrees() - start).rem_euclid(360.0) < sweep
                    })
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ComputedWidgetElement {
    pub index: usize,
    pub shape: WidgetShape,
    pub center: [f32; 2],
    pub label_box: WidgetElementRect,
    pub clip: WidgetElementRect,
}

impl ComputedWidgetElement {
    pub fn contains(self, p: [f32; 2]) -> bool {
        self.clip.contains(p) && self.shape.contains(p)
    }
}

/// Compatibility layout for old rectangle-only assets. Kept byte-for-byte in
/// its rounding/spacing rules; radial assets opt into the retained hierarchy.
pub fn widget_element_rects(
    resolution: [u32; 2],
    elements: &[WidgetElement],
) -> Vec<WidgetElementRect> {
    let w = resolution[0].max(1) as usize;
    let h = resolution[1].max(1) as usize;
    let pad = (w as f32 * 0.06).round() as usize;
    let inner_w = w.saturating_sub(2 * pad);
    let inner_h = h.saturating_sub(2 * pad);
    let visual: Vec<_> = elements
        .iter()
        .enumerate()
        .filter(|(_, e)| e.kind != WidgetElementKind::Container && e.opacity > 0.0)
        .collect();
    let n = visual.len().max(1);
    let horizontal = visual
        .first()
        .is_some_and(|(_, e)| e.parent_layout == WidgetLayout::Row);
    let mut ys: Vec<i32> = visual
        .iter()
        .filter_map(|(_, e)| e.position.map(|p| (p[1].clamp(0.0, 1.0) * 1000.0) as i32))
        .collect();
    ys.sort_unstable();
    ys.dedup();
    let abs_row_h = (h / ys.len().max(1)).max(8);
    visual
        .iter()
        .enumerate()
        .map(|(i, (index, el))| {
            let (x0, y0, x1, y1) = if let Some(p) = el.position {
                let x0 = (p[0].clamp(0.0, 1.0) * w as f32) as usize;
                let y0 = (p[1].clamp(0.0, 1.0) * h as f32) as usize;
                (
                    x0.min(w.saturating_sub(2)),
                    y0,
                    w.saturating_sub(pad / 2),
                    (y0 + abs_row_h).min(h),
                )
            } else if horizontal {
                let slot_w = (inner_w / n).max(2);
                let gap = (slot_w as f32 * 0.08).round() as usize;
                let x0 = pad + i * slot_w;
                (
                    x0,
                    pad,
                    (x0 + slot_w.saturating_sub(gap)).min(w.saturating_sub(pad)),
                    h.saturating_sub(pad),
                )
            } else {
                let slot = (inner_h / n).max(2);
                let gap = (slot as f32 * 0.25).round() as usize;
                let y0 = pad + i * slot;
                (
                    pad + (el.depth as usize * pad / 2).min(inner_w / 2),
                    y0,
                    w.saturating_sub(pad),
                    (y0 + slot.saturating_sub(gap).max(2)).min(h.saturating_sub(pad)),
                )
            };
            WidgetElementRect {
                index: *index,
                x0,
                y0,
                x1,
                y1,
            }
        })
        .collect()
}

/// Computes geometry in raster pixels, ordered by authored order then path.
/// Paths retain container ancestry even when children sort before their parent.
pub fn compute_widget_layout(
    resolution: [u32; 2],
    elements: &[WidgetElement],
) -> Vec<ComputedWidgetElement> {
    if !elements
        .iter()
        .any(|e| e.kind == WidgetElementKind::Container && e.layout == WidgetLayout::Radial)
    {
        return widget_element_rects(resolution, elements)
            .into_iter()
            .map(|r| ComputedWidgetElement {
                index: r.index,
                shape: WidgetShape::Rect(r),
                center: r.center(),
                label_box: r,
                clip: r,
            })
            .collect();
    }
    let root = WidgetElementRect {
        index: 0,
        x0: 0,
        y0: 0,
        x1: resolution[0].max(1) as usize,
        y1: resolution[1].max(1) as usize,
    };
    let mut out = Vec::new();
    // Iterative traversal bounds stack depth even for deeply nested authored prims.
    let mut pending = vec![("", WidgetLayout::Column, RadialSettings::default(), root)];
    while let Some((parent, layout, radial, bounds)) = pending.pop() {
        let mut children: Vec<_> = elements
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                e.opacity > 0.0 && e.path.rsplit_once('/').map_or("", |(p, _)| p) == parent
            })
            .collect();
        children.sort_by(|a, b| {
            a.1.order
                .cmp(&b.1.order)
                .then_with(|| a.1.path.cmp(&b.1.path))
        });
        let count = children.len().max(1);
        let buttons = children
            .iter()
            .filter(|(_, e)| e.kind == WidgetElementKind::Button)
            .count();
        let mut slot = 0;
        let w = bounds.x1.saturating_sub(bounds.x0) as f32;
        let h = bounds.y1.saturating_sub(bounds.y0) as f32;
        for (ordinal, (index, el)) in children.into_iter().enumerate() {
            if layout == WidgetLayout::Radial {
                if el.kind != WidgetElementKind::Button {
                    continue;
                }
                let ordinal = slot;
                slot += 1;
                if !radial.valid() {
                    continue;
                }
                let step = radial.sweep_angle / buttons.max(1) as f32;
                if radial.gap_angle >= step {
                    continue;
                }
                let center = [
                    bounds.x0 as f32 + w * radial.center[0],
                    bounds.y0 as f32 + h * radial.center[1],
                ];
                let inner = w.min(h) * radial.inner_radius;
                let outer = w.min(h) * radial.outer_radius;
                // Preserve the separation at the inner circle. Without a hole,
                // use the outer ring as the reference to retain a nonzero gap.
                let reference_radius = if inner > 0.0 { inner } else { outer };
                let gap_half_width =
                    reference_radius * (radial.gap_angle.min(180.0) * 0.5).to_radians().sin();
                let start = radial.start_angle + ordinal as f32 * step;
                let shape = WidgetShape::AnnularSector {
                    center,
                    inner,
                    outer,
                    start,
                    sweep: step,
                    gap_half_width,
                };
                let angle = (start + step * 0.5).to_radians();
                let r = (inner + outer) * 0.5;
                let label_center = [center[0] + angle.sin() * r, center[1] - angle.cos() * r];
                let sweep = shape.angular_span(r).map_or(0.0, |(_, sweep)| sweep);
                // Conservative square inscribed in the sector, then clipped to it.
                let side = ((outer - inner) * 0.65)
                    .min(2.0 * r * (sweep.min(180.0) * 0.5).to_radians().sin() * 0.65);
                let label_box = rect(
                    index,
                    label_center[0] - side * 0.5,
                    label_center[1] - side * 0.5,
                    label_center[0] + side * 0.5,
                    label_center[1] + side * 0.5,
                    bounds,
                );
                let clip = rect(
                    index,
                    center[0] - outer,
                    center[1] - outer,
                    center[0] + outer,
                    center[1] + outer,
                    bounds,
                );
                out.push(ComputedWidgetElement {
                    index,
                    shape,
                    center: label_center,
                    label_box,
                    clip,
                });
                continue;
            }
            let (cols, rows) = match layout {
                WidgetLayout::Row => (count, 1),
                WidgetLayout::Stack => (1, 1),
                WidgetLayout::Grid => {
                    let c = (count as f32).sqrt().ceil() as usize;
                    (c, count.div_ceil(c))
                }
                _ => (1, count),
            };
            let (col, row) = if layout == WidgetLayout::Stack {
                (0, 0)
            } else {
                (ordinal % cols, ordinal / cols)
            };
            let x = bounds.x0 as f32 + w * col as f32 / cols as f32;
            let y = bounds.y0 as f32 + h * row as f32 / rows as f32;
            let mut r = rect(
                index,
                x,
                y,
                x + w / cols as f32,
                y + h / rows as f32,
                bounds,
            );
            if let Some(p) = el.position {
                r.x0 = (bounds.x0 + (p[0].clamp(0.0, 1.0) * w) as usize).min(r.x1);
                r.y0 = (bounds.y0 + (p[1].clamp(0.0, 1.0) * h) as usize).min(r.y1);
            }
            if el.kind == WidgetElementKind::Container {
                pending.push((&el.path, el.layout, el.radial, r));
            } else {
                out.push(ComputedWidgetElement {
                    index,
                    shape: WidgetShape::Rect(r),
                    center: r.center(),
                    label_box: r,
                    clip: r,
                });
            }
        }
    }
    out.sort_by(|a, b| {
        elements[a.index]
            .order
            .cmp(&elements[b.index].order)
            .then_with(|| elements[a.index].path.cmp(&elements[b.index].path))
    });
    out
}

fn rect(
    index: usize,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    clip: WidgetElementRect,
) -> WidgetElementRect {
    WidgetElementRect {
        index,
        x0: (x0.max(0.0) as usize).clamp(clip.x0, clip.x1),
        y0: (y0.max(0.0) as usize).clamp(clip.y0, clip.y1),
        x1: (x1.max(0.0) as usize).clamp(clip.x0, clip.x1),
        y1: (y1.max(0.0) as usize).clamp(clip.y0, clip.y1),
    }
}

/// Quantize UV to the same pixel-center sample as rasterization. UV=1 is outside.
pub fn widget_sample(resolution: [u32; 2], uv: [f32; 2]) -> Option<[f32; 2]> {
    uv.into_iter().all(|v| (0.0..1.0).contains(&v)).then(|| {
        [
            (uv[0] * resolution[0].max(1) as f32).floor() + 0.5,
            (uv[1] * resolution[1].max(1) as f32).floor() + 0.5,
        ]
    })
}

pub fn widget_hit_test(
    resolution: [u32; 2],
    elements: &[WidgetElement],
    u: f32,
    v: f32,
) -> Option<usize> {
    let p = widget_sample(resolution, [u, v])?;
    compute_widget_layout(resolution, elements)
        .iter()
        .rev()
        .find(|r| r.contains(p))
        .map(|r| r.index)
}

/// Panel occlusion is independent of handler registration and enabled state.
pub fn widget_surface_hit(
    resolution: [u32; 2],
    elements: &[WidgetElement],
    surface: &WidgetSurface,
    uv: [f32; 2],
) -> bool {
    let Some(p) = widget_sample(resolution, uv) else {
        return false;
    };
    let sample_uv = [
        p[0] / resolution[0].max(1) as f32,
        p[1] / resolution[1].max(1) as f32,
    ];
    surface.contains(sample_uv)
        && (surface.opacity > 0.0 || widget_hit_test(resolution, elements, uv[0], uv[1]).is_some())
}

/// Reusable stick selector. Positive stick Y is up. Disabled slots remain in
/// place but cannot focus. Confirmation is deliberately a separate operation.
pub fn radial_stick_target(
    layout: &[ComputedWidgetElement],
    elements: &[WidgetElement],
    container: &str,
    stick: [f32; 2],
    previous: Option<usize>,
) -> Option<usize> {
    let settings = elements
        .iter()
        .find(|e| e.path == container && e.layout == WidgetLayout::Radial)?
        .radial;
    if !settings.valid()
        || !stick.iter().all(|v| v.is_finite())
        || stick[0].hypot(stick[1]) <= settings.dead_zone
    {
        return None;
    }
    let angle = stick[0].atan2(stick[1]).to_degrees();
    // Evaluate stick angles on the label ring, using the same inset geometry as
    // pointer picking rather than treating the untrimmed slot as clickable.
    let span = |r: &ComputedWidgetElement| match r.shape {
        WidgetShape::AnnularSector { inner, outer, .. } => {
            r.shape.angular_span((inner + outer) * 0.5)
        }
        _ => None,
    };
    let candidates = || {
        layout.iter().filter(|r| {
            elements[r.index].enabled
                && elements[r.index]
                    .path
                    .rsplit_once('/')
                    .is_some_and(|(p, _)| p == container)
        })
    };
    if let Some(r) = candidates().find(|r| Some(r.index) == previous) {
        if let Some((start, sweep)) = span(r) {
            let margin = settings.hysteresis_angle.min((360.0 - sweep) * 0.5);
            if (angle - start + margin).rem_euclid(360.0) < sweep + 2.0 * margin {
                return Some(r.index);
            }
        }
    }
    candidates()
        .find(|r| span(r).is_some_and(|(start, sweep)| (angle - start).rem_euclid(360.0) < sweep))
        .map(|r| r.index)
}

/// Draw/pick geometry in a common world frame. Supply the same central camera
/// basis for both stereo eyes, never an eye-specific logical panel surface.
pub fn resolve_widget_quad(
    world: Mat4,
    size: [f32; 2],
    billboard: WidgetBillboard,
    cam_right: Vec3,
    cam_up: Vec3,
) -> (Vec3, Vec3, Vec3) {
    let center = world.w_axis.truncate();
    let a = world.transform_vector3(Vec3::X) * size[0] * 0.5;
    let b = world.transform_vector3(Vec3::Y) * size[1] * 0.5;
    match billboard {
        WidgetBillboard::None => (center, a, b),
        WidgetBillboard::ScreenAligned => (
            center,
            cam_right.normalize_or_zero() * a.length(),
            cam_up.normalize_or_zero() * b.length(),
        ),
        WidgetBillboard::YAxis => {
            let right = Vec3::new(cam_right.x, 0.0, cam_right.z)
                .try_normalize()
                .unwrap_or(Vec3::X);
            (center, right * a.length(), Vec3::Y * b.length())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(path: &str, kind: WidgetElementKind, order: i32) -> WidgetElement {
        WidgetElement {
            path: path.into(),
            kind,
            order,
            ..Default::default()
        }
    }

    #[test]
    fn nested_menus_use_their_own_container_order_and_bounds() {
        let mut elements = vec![element("Root", WidgetElementKind::Container, 99)];
        elements[0].layout = WidgetLayout::Row;
        for (name, count) in [("A", 3), ("B", 5)] {
            let mut c = element(&format!("Root/{name}"), WidgetElementKind::Container, 10);
            c.layout = WidgetLayout::Radial;
            elements.push(c);
            for i in 0..count {
                elements.push(element(
                    &format!("Root/{name}/B{i}"),
                    WidgetElementKind::Button,
                    i,
                ));
            }
        }
        let before = compute_widget_layout([400, 200], &elements);
        assert_eq!(before.len(), 8);
        for e in &before {
            let WidgetShape::AnnularSector { center, outer, .. } = e.shape else {
                panic!("sector")
            };
            assert_eq!(outer, 100.0);
            assert_eq!(
                center,
                if elements[e.index].path.starts_with("Root/A/") {
                    [100.0, 100.0]
                } else {
                    [300.0, 100.0]
                }
            );
        }
        elements
            .iter_mut()
            .find(|e| e.path == "Root/A/B1")
            .unwrap()
            .enabled = false;
        assert_eq!(
            before,
            compute_widget_layout([400, 200], &elements),
            "disabled slots stay fixed"
        );
        let selected = radial_stick_target(&before, &elements, "Root/A", [0.0, -1.0], None);
        assert_eq!(selected, None, "disabled sector cannot focus");
    }

    #[test]
    fn sector_boundaries_and_dead_center_are_half_open() {
        let s = WidgetShape::AnnularSector {
            center: [0.0; 2],
            inner: 1.0,
            outer: 3.0,
            start: 0.0,
            sweep: 90.0,
            gap_half_width: 0.0,
        };
        assert!(s.contains([0.0, -1.0]), "inner and start edges included");
        assert!(!s.contains([0.0, -3.0]), "outer edge excluded");
        assert!(!s.contains([2.0, 0.0]), "end edge excluded");
        assert!(!s.contains([0.0; 2]));
        assert!(!s.contains([f32::NAN, 0.0]));
        let wrapped = WidgetShape::AnnularSector {
            center: [0.0; 2],
            inner: 0.0,
            outer: 3.0,
            start: 350.0,
            sweep: 20.0,
            gap_half_width: 0.0,
        };
        assert!(wrapped.contains([0.0, -2.0]));
        assert!(!wrapped.contains([0.0; 2]));
    }

    #[test]
    fn radial_gaps_have_constant_perpendicular_width() {
        for (count, sweep, start) in [(5, 360.0, -36.0), (3, 270.0, 350.0)] {
            let mut menu = element("Menu", WidgetElementKind::Container, 0);
            menu.layout = WidgetLayout::Radial;
            menu.radial.sweep_angle = sweep;
            menu.radial.start_angle = start;
            menu.radial.gap_angle = 12.0;
            let mut elements = vec![menu];
            for i in 0..count {
                elements.push(element(&format!("Menu/B{i}"), WidgetElementKind::Button, i));
            }
            let layout = compute_widget_layout([400, 240], &elements);
            let WidgetShape::AnnularSector {
                center,
                inner,
                outer,
                gap_half_width,
                ..
            } = layout[0].shape
            else {
                panic!("expected sector");
            };
            assert!((gap_half_width - inner * 6.0_f32.to_radians().sin()).abs() < 1e-5);
            let first_boundary = if sweep == 360.0 { 0 } else { 1 };
            for boundary in first_boundary..count {
                let angle = (start + boundary as f32 * sweep / count as f32).to_radians();
                let direction = [angle.sin(), -angle.cos()];
                let perpendicular = [angle.cos(), angle.sin()];
                for distance in [inner * 1.02, (inner + outer) * 0.5, outer * 0.95] {
                    let hit = |offset: f32| {
                        let p = [
                            center[0] + direction[0] * distance + perpendicular[0] * offset,
                            center[1] + direction[1] * distance + perpendicular[1] * offset,
                        ];
                        layout.iter().find(|e| e.contains(p)).map(|e| e.index)
                    };
                    assert_eq!(hit(0.0), None);
                    assert_eq!(hit(gap_half_width - 0.05), None);
                    assert_eq!(hit(-gap_half_width + 0.05), None);
                    assert_eq!(hit(gap_half_width + 0.05), Some(boundary as usize + 1));
                    let previous = (boundary + count - 1) % count;
                    assert_eq!(hit(-gap_half_width - 0.05), Some(previous as usize + 1));
                }
            }
        }
    }

    #[test]
    fn radial_gap_without_inner_hole_remains_nonzero() {
        let mut menu = element("Menu", WidgetElementKind::Container, 0);
        menu.layout = WidgetLayout::Radial;
        menu.radial.inner_radius = 0.0;
        menu.radial.gap_angle = 10.0;
        let elements = vec![
            menu,
            element("Menu/A", WidgetElementKind::Button, 0),
            element("Menu/B", WidgetElementKind::Button, 1),
        ];
        let layout = compute_widget_layout([200; 2], &elements);
        assert!(layout.iter().all(|e| !e.contains([100.0, 100.0])));
        assert!(layout.iter().all(|e| !e.contains([100.0, 20.0])));
        assert!(layout[0].contains([120.0, 20.0]));
    }

    #[test]
    fn polygon_concavity_holes_winding_and_invalid_topology() {
        // Two disjoint rectangles leave a central hole; reversing winding has
        // no effect. The entire rectangular bounding quad is never substituted.
        let v = vec![
            [0.0, 0.0],
            [0.25, 0.0],
            [0.25, 1.0],
            [0.0, 1.0],
            [0.75, 0.0],
            [1.0, 0.0],
            [1.0, 1.0],
            [0.75, 1.0],
        ];
        let p = WidgetPolygon::new(v.clone(), vec![0, 1, 2, 0, 2, 3, 6, 5, 4, 7, 6, 4]).unwrap();
        assert!(p.contains([0.1, 0.5]));
        assert!(p.contains([0.9, 0.5]));
        assert!(!p.contains([0.5, 0.5]));
        assert!(WidgetPolygon::new(v.clone(), vec![0, 1, 8]).is_err());
        assert!(WidgetPolygon::new(v.clone(), vec![0, 1]).is_err());
        assert!(WidgetPolygon::new(v, vec![0, 0, 1]).is_err());
        assert!(WidgetPolygon::new(vec![[f32::NAN, 0.0]; 3], vec![0, 1, 2]).is_err());
        assert!(
            !WidgetSurface {
                outline: Some(WidgetPolygon::default()),
                ..Default::default()
            }
            .contains([0.5; 2])
        );
    }

    #[test]
    fn stick_deadzone_partial_sweep_and_boundary_hysteresis() {
        let mut c = element("Menu", WidgetElementKind::Container, 0);
        c.layout = WidgetLayout::Radial;
        c.radial.sweep_angle = 180.0;
        c.radial.gap_angle = 0.0;
        let els = vec![
            c,
            element("Menu/A", WidgetElementKind::Button, 0),
            element("Menu/B", WidgetElementKind::Button, 1),
        ];
        let layout = compute_widget_layout([200; 2], &els);
        let stick = |degrees: f32| [degrees.to_radians().sin(), degrees.to_radians().cos()];
        assert_eq!(
            radial_stick_target(&layout, &els, "Menu", [0.1; 2], Some(1)),
            None
        );
        assert_eq!(
            radial_stick_target(&layout, &els, "Menu", stick(91.0), Some(1)),
            Some(1)
        );
        assert_eq!(
            radial_stick_target(&layout, &els, "Menu", stick(96.0), Some(1)),
            Some(2)
        );
        assert_eq!(
            radial_stick_target(&layout, &els, "Menu", stick(270.0), None),
            None
        );
    }
}
