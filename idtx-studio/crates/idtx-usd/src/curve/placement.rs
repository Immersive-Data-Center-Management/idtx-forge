//! Deterministic placement planning with conservative full-footprint clearance.
use super::*;
use sha2::{Digest, Sha256};

pub const MAX_PLACEMENTS: usize = 4096;

#[derive(Debug, Clone, PartialEq)]
pub struct PlacementSettings {
    pub curve_index: u32,
    pub spacing: f64,
    pub start: f64,
    /// Finite reveal window in metres, measured from start.
    pub window: f64,
    pub lateral: [f64; 2],
    pub floor_normal: [f64; 3],
    /// Full object clearance: lateral/up/forward half extents, in metres.
    pub half_extents: [f64; 3],
    pub orient: bool,
    pub seed: u32,
}

impl Default for PlacementSettings {
    fn default() -> Self {
        Self {
            curve_index: 0,
            spacing: 0.25,
            start: 0.0,
            window: 10.0,
            lateral: [-0.08, 0.08],
            floor_normal: [0.0, 1.0, 0.0],
            half_extents: [0.06, 0.02, 0.1],
            orient: true,
            seed: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClearanceObstacle {
    pub path: String,
    pub minimum: DVec3,
    pub maximum: DVec3,
}

#[derive(Debug, Clone)]
pub struct PlacementPreview {
    /// Normalized Y-up metre world poses. +Z follows the flattened tangent.
    pub transforms: Vec<DMat4>,
    pub collisions: Vec<(usize, String)>,
    pub source_digest: String,
    pub length: f64,
    pub diagnostics: Vec<String>,
}

pub fn source_digest(source: &BasisCurvesRecord, time: Option<f64>) -> String {
    // Explicit endian-stable encoding, excluding ephemeral PrimId/revision.
    let mut hash = Sha256::new();
    hash.update(GENERATOR_VERSION);
    hash.update([u8::from(source.eligible)]);
    for text in
        [&source.curve_type, &source.basis, &source.wrap, &source.start_group, &source.end_group]
    {
        hash.update((text.len() as u64).to_le_bytes());
        hash.update(text.as_bytes());
    }
    for p in source.points.iter().flatten() {
        hash.update(p.to_le_bytes());
    }
    for n in &source.counts {
        hash.update(n.to_le_bytes());
    }
    for x in source.world_matrix {
        hash.update(x.to_le_bytes());
    }
    hash.update(source.metres_per_unit.to_le_bytes());
    hash.update([u8::from(source.z_up)]);
    hash.update(source.tolerance.to_le_bytes());
    hash.update([u8::from(time.is_some())]);
    hash.update(time.unwrap_or(0.0).to_le_bytes());
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

impl PlacementSettings {
    pub fn digest(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(self.curve_index.to_le_bytes());
        hash.update(self.seed.to_le_bytes());
        hash.update([u8::from(self.orient)]);
        for x in [self.spacing, self.start, self.window]
            .into_iter()
            .chain(self.lateral)
            .chain(self.floor_normal)
            .chain(self.half_extents)
        {
            hash.update(x.to_le_bytes());
        }
        hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }
    pub fn validate(&self) -> Result<(), CurveError> {
        if [self.spacing, self.start, self.window]
            .into_iter()
            .chain(self.lateral)
            .chain(self.floor_normal)
            .chain(self.half_extents)
            .any(|v| !v.is_finite())
            || self.spacing <= 0.0
            || self.start < 0.0
            || self.window < 0.0
            || self.half_extents.iter().any(|&v| v <= 0.0)
            || !DVec3::from_array(self.floor_normal).length_squared().is_finite()
            || DVec3::from_array(self.floor_normal).length_squared() < 1e-12
        {
            return Err(CurveError::Invalid(
                "invalid spacing/window, floor normal or footprint".into(),
            ));
        }
        Ok(())
    }
}

pub fn preview(
    source: &BasisCurvesRecord,
    time: Option<f64>,
    settings: &PlacementSettings,
    obstacles: &[ClearanceObstacle],
) -> Result<PlacementPreview, CurveError> {
    settings.validate()?;
    if !source.eligible {
        return Err(CurveError::Invalid(
            "route is authored ineligible (idtx:curve:eligible)".into(),
        ));
    }
    let curve = CurveEvaluator::new(source, settings.curve_index as usize)?;
    if settings.start > curve.length() {
        return Err(CurveError::Invalid("reveal start exceeds route length".into()));
    }
    let extent = settings.window.min(curve.length() - settings.start);
    let count = (extent / settings.spacing).floor() + 1.0;
    if count > MAX_PLACEMENTS as f64 {
        return Err(CurveError::Limit);
    }
    if (count as usize).saturating_mul(obstacles.len()) > 4_194_304 {
        return Err(CurveError::Limit);
    }
    let up = DVec3::from_array(settings.floor_normal).normalize();
    let fallback = (DVec3::Z - up * up.z).try_normalize().unwrap_or(DVec3::X);
    let mut result = PlacementPreview {
        transforms: Vec::new(),
        collisions: Vec::new(),
        source_digest: source_digest(source, time),
        length: curve.length(),
        diagnostics: Vec::new(),
    };
    for (name, group) in [("start", &source.start_group), ("end", &source.end_group)] {
        if group.is_empty() {
            result.diagnostics.push(format!("{name} endpoint group is unassigned"));
        }
    }
    if curve.length() == 0.0 {
        result.diagnostics.push("zero-length route: one stable placement".into());
    }
    for i in 0..count as usize {
        let sample = curve.sample_distance(
            settings.start + i as f64 * settings.spacing,
            CurveSpace::StageWorldMetres,
        )?;
        let projected = sample.tangent - up * sample.tangent.dot(up);
        let forward = if projected.length_squared() > 1e-12 {
            projected.normalize()
        } else {
            fallback
        };
        let right = up.cross(forward).normalize();
        let position =
            sample.point + right * settings.lateral[(i + settings.seed as usize % 2) % 2];
        let matrix = if settings.orient {
            DMat4::from_cols(
                right.extend(0.0),
                up.extend(0.0),
                forward.extend(0.0),
                position.extend(1.0),
            )
        } else {
            DMat4::from_translation(position)
        };
        let mut min = DVec3::splat(f64::INFINITY);
        let mut max = -min;
        for x in [-1.0, 1.0] {
            for y in [-1.0, 1.0] {
                for z in [-1.0, 1.0] {
                    // Placement origin rests on the floor; clearance extends upward.
                    let p = matrix.transform_point3(DVec3::new(
                        x * settings.half_extents[0],
                        (y + 1.0) * settings.half_extents[1],
                        z * settings.half_extents[2],
                    ));
                    min = min.min(p);
                    max = max.max(p);
                }
            }
        }
        if !min.as_vec3().is_finite() || !max.as_vec3().is_finite() {
            return Err(CurveError::Invalid("footprint exceeds finite preview coordinates".into()));
        }
        for obstacle in obstacles {
            if min.cmplt(obstacle.maximum).all()
                && max.cmpgt(obstacle.minimum).all()
                && result.collisions.len() < MAX_PLACEMENTS
            {
                result.collisions.push((i, obstacle.path.clone()));
            }
        }
        result.transforms.push(matrix);
    }
    Ok(result)
}
