//! Editor curve guides, shared by Inspector, Poser and Conductor.
use crate::OverlayLine;
use glam::Vec3;
use idtx_scene::CurveComp;
use idtx_usd::curve::CurveSpace;

/// Bounded tessellated guide plus green start/red end ticks and direction arrow.
/// `metres_to_render` accounts only for the configured renderer world unit.
pub fn push_curve_guide(
    lines: &mut Vec<OverlayLine>,
    curve: &CurveComp,
    selected: bool,
    metres_to_render: f32,
) {
    if curve.source.prototype_only {
        return;
    }
    let map = |p: glam::DVec3| p.as_vec3() * metres_to_render;
    let mut line = |a: Vec3, b: Vec3, color| {
        if lines.len() < 65_536 {
            lines.push(OverlayLine {
                a: a.to_array(),
                b: b.to_array(),
                color,
            });
        }
    };
    for table in curve.tables.iter().filter_map(|t| t.as_ref().ok()) {
        let mut points = table.guide_points();
        if let Some(mut previous) = points.next() {
            for point in points {
                line(map(previous), map(point), [0.25, 0.8, 1.0, 1.0]);
                previous = point;
            }
        }
        for (distance, color) in
            [(0.0, [0.2, 1.0, 0.3, 1.0]), (table.length(), [1.0, 0.25, 0.2, 1.0])]
        {
            if let Ok(sample) = table.sample_distance(distance, CurveSpace::StageWorldMetres) {
                let p = map(sample.point);
                let r = 0.035 * metres_to_render;
                line(p - Vec3::X * r, p + Vec3::X * r, color);
                line(p - Vec3::Y * r, p + Vec3::Y * r, color);
                line(p - Vec3::Z * r, p + Vec3::Z * r, color);
            }
        }
        if let Ok(sample) =
            table.sample_distance(table.length() * 0.5, CurveSpace::StageWorldMetres)
        {
            let p = map(sample.point);
            let forward = sample.tangent.as_vec3();
            let side = forward.cross(Vec3::Y).try_normalize().unwrap_or(Vec3::X);
            let r = 0.08 * metres_to_render;
            line(p, p - forward * r + side * r * 0.5, [1.0, 0.85, 0.2, 1.0]);
            line(p, p - forward * r - side * r * 0.5, [1.0, 0.85, 0.2, 1.0]);
        }
    }
    if selected {
        let matrix = curve.source.local_to_metres();
        let mut offset = 0;
        for &n in &curve.source.counts {
            let Ok(n) = usize::try_from(n) else {
                break;
            };
            let Some(points) = curve.source.points.get(offset..offset + n) else {
                break;
            };
            for pair in points.windows(2) {
                line(
                    map(matrix.transform_point3(glam::DVec3::from_array(pair[0].map(f64::from)))),
                    map(matrix.transform_point3(glam::DVec3::from_array(pair[1].map(f64::from)))),
                    [0.7, 0.65, 0.25, 0.5],
                );
            }
            offset += n;
        }
    }
}
