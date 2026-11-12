use crate::proto::Matrix4dTransform;

/// Flat row-major USD `GfMatrix4d`: index = `row * 4 + col`.
/// USD uses the row-vector convention (`v' = v · M`); translation is in row 3
/// (indices 12–14).
pub type UsdMat4 = [f64; 16];

/// Convert a USD `GfMatrix4d` to the wire `Matrix4dTransform`.
///
/// The wire format uses a column-vector convention with translation on the
/// bottom row. Conversion: transpose the upper-left 3×3 block; leave the
/// translation row (row 3) and the right column (col 3) unchanged.
#[must_use]
pub fn wire_from_usd(m: UsdMat4) -> Matrix4dTransform {
    Matrix4dTransform {
        // row 0 of wire = col 0 of USD 3×3 + m03 unchanged
        m00: m[0],
        m01: m[4],
        m02: m[8],
        m03: m[3],
        // row 1 of wire = col 1 of USD 3×3 + m13 unchanged
        m10: m[1],
        m11: m[5],
        m12: m[9],
        m13: m[7],
        // row 2 of wire = col 2 of USD 3×3 + m23 unchanged
        m20: m[2],
        m21: m[6],
        m22: m[10],
        m23: m[11],
        // translation row unchanged
        m30: m[12],
        m31: m[13],
        m32: m[14],
        m33: m[15],
    }
}

/// Convert a wire `Matrix4dTransform` back to a USD `GfMatrix4d`.
///
/// Inverse of [`wire_from_usd`]: transpose the upper-left 3×3 back; translation
/// row is preserved unchanged.
#[must_use]
pub fn usd_from_wire(w: &Matrix4dTransform) -> UsdMat4 {
    [
        w.m00, w.m10, w.m20, w.m03, w.m01, w.m11, w.m21, w.m13, w.m02, w.m12, w.m22, w.m23, w.m30,
        w.m31, w.m32, w.m33,
    ]
}

#[cfg(test)]
mod tests {
    use super::{UsdMat4, usd_from_wire, wire_from_usd};

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn mats_approx_eq(a: &UsdMat4, b: &UsdMat4) -> bool {
        a.iter().zip(b.iter()).all(|(x, y)| approx_eq(*x, *y))
    }

    #[test]
    fn round_trip_identity() {
        #[rustfmt::skip]
        let identity: UsdMat4 = [
            1.0, 0.0, 0.0, 0.0,
            0.0, 1.0, 0.0, 0.0,
            0.0, 0.0, 1.0, 0.0,
            0.0, 0.0, 0.0, 1.0,
        ];
        let back = usd_from_wire(&wire_from_usd(identity));
        assert!(
            mats_approx_eq(&identity, &back),
            "identity round-trip: {back:?}"
        );
    }

    #[test]
    fn round_trip_translation() {
        #[rustfmt::skip]
        let mat: UsdMat4 = [
            1.0, 0.0, 0.0, 0.0,
            0.0, 1.0, 0.0, 0.0,
            0.0, 0.0, 1.0, 0.0,
            3.0, 4.0, 5.0, 1.0,
        ];
        let wire = wire_from_usd(mat);
        // Translation must appear on the wire's bottom row unchanged.
        assert!(approx_eq(wire.m30, 3.0));
        assert!(approx_eq(wire.m31, 4.0));
        assert!(approx_eq(wire.m32, 5.0));
        let back = usd_from_wire(&wire);
        assert!(
            mats_approx_eq(&mat, &back),
            "translation round-trip: {back:?}"
        );
    }

    /// The upper-left 3×3 of the USD matrix must be transposed on the wire.
    /// USD row-vector 90° CCW around Z: right=(0,1,0), up=(-1,0,0), fwd=(0,0,1).
    #[test]
    fn wire_from_usd_transposes_3x3() {
        #[rustfmt::skip]
        let mat: UsdMat4 = [
             0.0,  1.0, 0.0, 0.0,   // row 0 (right vector)
            -1.0,  0.0, 0.0, 0.0,   // row 1 (up vector)
             0.0,  0.0, 1.0, 0.0,   // row 2 (fwd vector)
             0.0,  0.0, 0.0, 1.0,
        ];
        let w = wire_from_usd(mat);
        // col 0 of USD rows [0.0, -1.0, 0.0] → wire row 0
        assert!(approx_eq(w.m00, 0.0));
        assert!(approx_eq(w.m01, -1.0));
        assert!(approx_eq(w.m02, 0.0));
        // col 1 of USD rows [1.0, 0.0, 0.0] → wire row 1
        assert!(approx_eq(w.m10, 1.0));
        assert!(approx_eq(w.m11, 0.0));
        assert!(approx_eq(w.m12, 0.0));
    }

    #[test]
    fn round_trip_rotation_and_translation() {
        // Arbitrary non-trivial matrix: scale + rotation + translation.
        #[rustfmt::skip]
        let mat: UsdMat4 = [
             0.0,  2.0,  0.0, 0.0,
            -2.0,  0.0,  0.0, 0.0,
             0.0,  0.0,  2.0, 0.0,
            10.0, 20.0, 30.0, 1.0,
        ];
        let back = usd_from_wire(&wire_from_usd(mat));
        assert!(
            mats_approx_eq(&mat, &back),
            "rotation+translation round-trip: {back:?}"
        );
    }
}
