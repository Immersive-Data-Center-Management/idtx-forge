//! Native BasisCurves evaluation shared by the editor, baker and runtime queries.
//!
//! Distances are always metres in normalized (Y-up) stage-world space. An
//! adaptive de Casteljau table bounds total length error by `tolerance / 2`:
//! every leaf's control-polygon minus chord length must fit its share of the
//! tolerance. Distance inversion uses the same bounds, not uniform parameters.
//! Limits are hard errors, never silently degraded accuracy. Zero-length paths
//! have length zero, return their first point and a deterministic +X tangent.

use crate::PrimId;
use glam::{DMat4, DVec3};
pub mod placement;

pub const MAX_CURVE_POINTS: usize = 16_384;
pub const MAX_CURVE_SAMPLES: usize = 16_384;
pub const MAX_QUERY_SAMPLES: usize = 256;
pub const GENERATOR_VERSION: &str = "idtx-curve-placement/1";

#[derive(Debug, Clone, PartialEq)]
pub struct BasisCurvesRecord {
    pub prim: PrimId,
    pub points: Vec<[f32; 3]>,
    pub counts: Vec<i32>,
    pub curve_type: String,
    pub basis: String,
    pub wrap: String,
    /// OpenUSD row-vector matrix, stored row-major. Equivalent glam column data.
    pub world_matrix: [f64; 16],
    pub metres_per_unit: f64,
    pub z_up: bool,
    pub tolerance: f64,
    pub prototype_only: bool,
    pub eligible: bool,
    pub start_group: String,
    pub end_group: String,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CurveError {
    #[error("unsupported native curve type/basis/wrap: {0}")]
    Unsupported(String),
    #[error("invalid curve: {0}")]
    Invalid(String),
    #[error("curve index out of range")]
    Index,
    #[error("curve evaluation limit exceeded; increase tolerance or simplify the path")]
    Limit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveSpace {
    Local,
    StageWorldMetres,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct CurveSample {
    pub point: DVec3,
    pub tangent: DVec3,
}

type Cubic = [DVec3; 4];

#[derive(Debug, Clone)]
struct Leaf {
    local: Cubic,
    world: Cubic,
    start: f64,
    length: f64,
}

#[derive(Debug, Clone)]
pub struct CurveEvaluator {
    leaves: Vec<Leaf>,
    length: f64,
    tolerance: f64,
    fallback_local: DVec3,
    fallback_world: DVec3,
    pub periodic: bool,
}

pub fn normalized_stage_matrix(metres_per_unit: f64, z_up: bool) -> DMat4 {
    DMat4::from_scale(DVec3::splat(metres_per_unit))
        * if z_up {
            DMat4::from_rotation_x(-std::f64::consts::FRAC_PI_2)
        } else {
            DMat4::IDENTITY
        }
}

impl BasisCurvesRecord {
    pub fn geometry_eq(&self, other: &Self) -> bool {
        self.prim == other.prim
            && self.points == other.points
            && self.counts == other.counts
            && self.curve_type == other.curve_type
            && self.basis == other.basis
            && self.wrap == other.wrap
            && self.world_matrix == other.world_matrix
            && self.metres_per_unit == other.metres_per_unit
            && self.z_up == other.z_up
            && self.tolerance == other.tolerance
            && self.prototype_only == other.prototype_only
    }
    pub fn local_to_metres(&self) -> DMat4 {
        normalized_stage_matrix(self.metres_per_unit, self.z_up)
            * DMat4::from_cols_array(&self.world_matrix)
    }

    pub fn validate(&self) -> Result<(), CurveError> {
        crate::validate_usd_affine(&self.world_matrix)
            .map_err(|e| CurveError::Invalid(e.to_string()))?;
        if self.points.len() > MAX_CURVE_POINTS || self.counts.len() > MAX_CURVE_POINTS {
            return Err(CurveError::Limit);
        }
        if !matches!(self.wrap.as_str(), "nonperiodic" | "periodic")
            || !(self.curve_type == "linear"
                || self.curve_type == "cubic" && self.basis == "bezier")
        {
            return Err(CurveError::Unsupported(format!(
                "{}/{}/{}",
                self.curve_type, self.basis, self.wrap
            )));
        }
        if !self.tolerance.is_finite()
            || self.tolerance < 1e-9
            || self.tolerance > 1.0
            || !self.metres_per_unit.is_finite()
            || self.metres_per_unit <= 0.0
            || self.points.iter().flatten().any(|x| !x.is_finite())
            || self.world_matrix.iter().any(|x| !x.is_finite())
        {
            return Err(CurveError::Invalid(
                "non-finite data, invalid units or tolerance (1e-9..1 metre)".into(),
            ));
        }
        let mut total = 0usize;
        for &n in &self.counts {
            let n = usize::try_from(n)
                .map_err(|_| CurveError::Invalid("negative vertex count".into()))?;
            let valid = if self.curve_type == "linear" {
                n >= 2
            } else if self.wrap == "periodic" {
                n >= 3 && n % 3 == 0
            } else {
                n >= 4 && (n - 1) % 3 == 0
            };
            if !valid {
                return Err(CurveError::Invalid("linear needs >=2 vertices; Bezier needs 3n+1 (nonperiodic) or 3n (periodic) per curve".into()));
            }
            total = total.checked_add(n).ok_or(CurveError::Limit)?;
        }
        if self.counts.is_empty() || total != self.points.len() {
            return Err(CurveError::Invalid(
                "curveVertexCounts must exactly partition points".into(),
            ));
        }
        Ok(())
    }
}

fn split(p: Cubic, t: f64) -> (Cubic, Cubic) {
    let a = p[0].lerp(p[1], t);
    let b = p[1].lerp(p[2], t);
    let c = p[2].lerp(p[3], t);
    let d = a.lerp(b, t);
    let e = b.lerp(c, t);
    let f = d.lerp(e, t);
    ([p[0], a, d, f], [f, e, c, p[3]])
}

fn bounds(p: Cubic) -> (f64, f64) {
    (p[0].distance(p[3]), p.windows(2).map(|v| v[0].distance(v[1])).sum())
}

fn fallback(mut points: impl Iterator<Item = DVec3>) -> DVec3 {
    points.find_map(|p| p.try_normalize()).unwrap_or(DVec3::X)
}

impl CurveEvaluator {
    pub fn new(record: &BasisCurvesRecord, index: usize) -> Result<Self, CurveError> {
        record.validate()?;
        let &count = record.counts.get(index).ok_or(CurveError::Index)?;
        let offset: usize = record.counts[..index].iter().map(|&n| n as usize).sum();
        let points: Vec<_> = record.points[offset..offset + count as usize]
            .iter()
            .map(|&p| DVec3::from_array(p.map(f64::from)))
            .collect();
        let periodic = record.wrap == "periodic";
        let segments = if record.curve_type == "linear" {
            points.len() - usize::from(!periodic)
        } else {
            (points.len() - usize::from(!periodic)) / 3
        };
        let matrix = record.local_to_metres();
        let world_magnitude = points
            .iter()
            .map(|&p| matrix.transform_point3(p).abs().max_element())
            .fold(0.0, f64::max);
        if !world_magnitude.is_finite() || world_magnitude * f64::EPSILON * 64.0 > record.tolerance
        {
            return Err(CurveError::Invalid(
                "world coordinates cannot represent the requested tolerance".into(),
            ));
        }
        let mut result = Self {
            leaves: Vec::new(),
            length: 0.0,
            tolerance: record.tolerance,
            fallback_local: fallback(points.windows(2).map(|v| v[1] - v[0])),
            fallback_world: fallback(
                points.windows(2).map(|v| matrix.transform_vector3(v[1] - v[0])),
            ),
            periodic,
        };
        for segment in 0..segments {
            let local = if record.curve_type == "linear" {
                let a = points[segment];
                let b = points[(segment + 1) % points.len()];
                [a, a.lerp(b, 1.0 / 3.0), a.lerp(b, 2.0 / 3.0), b]
            } else {
                std::array::from_fn(|i| points[(segment * 3 + i) % points.len()])
            };
            let world = local.map(|p| matrix.transform_point3(p));
            result.subdivide(local, world, record.tolerance / segments as f64, 0)?;
        }
        Ok(result)
    }

    fn subdivide(
        &mut self,
        local: Cubic,
        world: Cubic,
        budget: f64,
        depth: u32,
    ) -> Result<(), CurveError> {
        if self.leaves.len() >= MAX_CURVE_SAMPLES {
            return Err(CurveError::Limit);
        }
        let (lower, upper) = bounds(world);
        if !upper.is_finite() {
            return Err(CurveError::Invalid("transformed length is not finite".into()));
        }
        if upper - lower <= budget {
            let length = (lower + upper) * 0.5;
            self.leaves.push(Leaf {
                local,
                world,
                start: self.length,
                length,
            });
            self.length += length;
            return Ok(());
        }
        if depth >= 20 {
            return Err(CurveError::Limit);
        }
        let (la, lb) = split(local, 0.5);
        let (wa, wb) = split(world, 0.5);
        self.subdivide(la, wa, budget * 0.5, depth + 1)?;
        self.subdivide(lb, wb, budget * 0.5, depth + 1)
    }

    pub fn length(&self) -> f64 {
        self.length
    }
    pub fn tolerance(&self) -> f64 {
        self.tolerance
    }
    pub fn table_samples(&self) -> usize {
        self.leaves.len()
    }

    /// Clamps finite distances to [0,length], including periodic curves. Callers
    /// wanting looping explicitly use rem_euclid(length). Tangents are unit vectors.
    pub fn sample_distance(
        &self,
        distance: f64,
        space: CurveSpace,
    ) -> Result<CurveSample, CurveError> {
        if !distance.is_finite() {
            return Err(CurveError::Invalid("distance must be finite".into()));
        }
        let distance = distance.clamp(0.0, self.length);
        let index = self
            .leaves
            .partition_point(|l| l.start + l.length < distance)
            .min(self.leaves.len() - 1);
        let leaf = &self.leaves[index];
        let target = distance - leaf.start;
        let t = if target <= 0.0 {
            0.0
        } else if target >= leaf.length {
            1.0
        } else {
            let (mut lo, mut hi) = (0.0, 1.0);
            // 52 bisections bound floating-point inversion even for very long
            // straight segments with highly nonuniform parameter speed.
            for _ in 0..52 {
                let mid = (lo + hi) * 0.5;
                let (a, _) = split(leaf.world, mid);
                let (l, u) = bounds(a);
                if (l + u) * 0.5 < target {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            (lo + hi) * 0.5
        };
        let (p, fallback) = match space {
            CurveSpace::Local => (leaf.local, self.fallback_local),
            CurveSpace::StageWorldMetres => (leaf.world, self.fallback_world),
        };
        let (left, _) = split(p, t);
        let tangent = ((p[1] - p[0]) * (1.0 - t).powi(2)
            + (p[2] - p[1]) * 2.0 * (1.0 - t) * t
            + (p[3] - p[2]) * t * t)
            .try_normalize()
            .unwrap_or_else(|| {
                // At repeated endpoint controls prefer the one-sided direction.
                let candidates = if t > 0.5 {
                    [p[3] - p[2], p[3] - p[1], p[3] - p[0]]
                } else {
                    [p[1] - p[0], p[2] - p[0], p[3] - p[0]]
                };
                candidates.into_iter().find_map(|v| v.try_normalize()).unwrap_or(fallback)
            });
        Ok(CurveSample {
            point: left[3],
            tangent,
        })
    }

    /// Bounded, allocation-free query; rejects a short buffer without writing.
    pub fn sample_distances(
        &self,
        distances: &[f64],
        space: CurveSpace,
        out: &mut [CurveSample],
    ) -> Result<(), CurveError> {
        if distances.len() > MAX_QUERY_SAMPLES || out.len() < distances.len() {
            return Err(CurveError::Limit);
        }
        if distances.iter().any(|d| !d.is_finite()) {
            return Err(CurveError::Invalid("distance must be finite".into()));
        }
        for (d, out) in distances.iter().zip(out) {
            *out = self.sample_distance(*d, space)?;
        }
        Ok(())
    }

    /// Adaptive polyline for editor overlays, in normalized stage-world metres.
    pub fn guide_points(&self) -> impl Iterator<Item = DVec3> + '_ {
        self.leaves
            .first()
            .map(|l| l.world[0])
            .into_iter()
            .chain(self.leaves.iter().map(|l| l.world[3]))
    }
}
