//! USD row-vector affine transform conversion without lossy decomposition.
use crate::{StageError, TransformValues};
use glam::{DMat3, DMat4, DQuat, DVec3, EulerRot};

/// Validate finite affine data in flat USD row-major layout.
///
/// # Errors
/// Rejects non-finite or projective matrices; reflected/collapsed local axes are valid.
#[allow(clippy::float_cmp)] // Exact homogeneous layout; near-affine is projective.
pub fn validate_usd_affine(matrix: &[f64; 16]) -> Result<(), StageError> {
    if !matrix.iter().all(|v| v.is_finite()) {
        return Err(StageError::InvalidTransform("matrix must be finite".into()));
    }
    if matrix[3] != 0.0 || matrix[7] != 0.0 || matrix[11] != 0.0 || matrix[15] != 1.0 {
        return Err(StageError::InvalidTransform("matrix must be affine".into()));
    }
    Ok(())
}

/// Construct USD's `scale * rotateX * rotateY * rotateZ * translate` matrix.
/// The equivalent column-vector order is `T * Rz * Ry * Rx * S`.
///
/// # Errors
/// Rejects non-finite inputs or overflow during composition.
pub fn usd_trs_matrix(values: TransformValues) -> Result<[f64; 16], StageError> {
    if !values.translation.iter().chain(&values.rotation_xyz_degrees).chain(&values.scale).all(|v| v.is_finite()) {
        return Err(StageError::InvalidTransform("TRS values must be finite".into()));
    }
    let [rx, ry, rz] = values.rotation_xyz_degrees.map(f64::to_radians);
    let rotation = DQuat::from_euler(EulerRot::ZYX, rz, ry, rx);
    let matrix = DMat4::from_scale_rotation_translation(DVec3::from_array(values.scale), rotation, DVec3::from_array(values.translation)).to_cols_array();
    validate_usd_affine(&matrix)?;
    Ok(matrix)
}

/// Convert a world matrix relative to a parent using full double precision.
///
/// # Errors
/// Rejects invalid input, singular/near-collinear parent axes, or overflow.
pub fn usd_world_to_local(world: &[f64; 16], parent: &[f64; 16]) -> Result<[f64; 16], StageError> {
    validate_usd_affine(world)?;
    validate_usd_affine(parent)?;
    let parent = DMat4::from_cols_array(parent);
    let axes = [parent.x_axis.truncate(), parent.y_axis.truncate(), parent.z_axis.truncate()];
    let lengths = axes.map(DVec3::length);
    if lengths.iter().any(|v| *v == 0.0 || !v.is_finite()) {
        return Err(StageError::InvalidTransform("singular parent transform".into()));
    }
    let normalized = DMat3::from_cols(axes[0] / lengths[0], axes[1] / lengths[1], axes[2] / lengths[2]);
    if normalized.determinant().abs() <= 1e-12 {
        return Err(StageError::InvalidTransform("singular parent transform".into()));
    }
    let mut local = (parent.inverse() * DMat4::from_cols_array(world)).to_cols_array();
    // Inversion can round the homogeneous component. Affine structure is known
    // from the validated operands; restore it without altering the 3D mapping.
    local[3] = 0.0;
    local[7] = 0.0;
    local[11] = 0.0;
    local[15] = 1.0;
    validate_usd_affine(&local)?;
    Ok(local)
}
