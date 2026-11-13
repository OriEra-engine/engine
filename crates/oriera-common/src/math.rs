//! L0 math vocabulary — vector/matrix types shared by every layer above.
//!
//! Issue #7 asked for `Vec3` and `Transform` in this crate, with one
//! constraint that decides the whole design: **do not build a second
//! math library**. So `Vec3`, `Quat`, `Mat3`, `Mat4` are glam's types
//! re-exported verbatim — one implementation, SIMD where the target
//! allows it, and no conversion shim when a value travels L3 (object
//! model) → L5 (render) → L6 (physics).
//!
//! `Transform` is the only project-specific type: the TRS triple that
//! scenes, assets and physics bodies all need, with one documented
//! order — **scale, then rotation, then translation** — spelled by
//! [`Mat4::from_scale_rotation_translation`].
//!
//! Everything public here is `Debug + Clone + Serialize` (glam's serde
//! feature is on workspace-wide), so a `Transform` can ride in a scene
//! spec or a run manifest without a per-layer encode.

use serde::{Deserialize, Serialize};

pub use glam::{Mat3, Mat4, Quat, Vec2, Vec3};

/// A rigid-ish placement: uniform-or-anisotropic scale, a rotation, and
/// a translation.
///
/// Composition order is fixed and part of the contract: a point `p` maps
/// to `translation + rotation * (scale * p)`. Feed the three fields
/// straight into [`Mat4::from_scale_rotation_translation`]; do not
/// hand-roll the product elsewhere — that is how two layers end up with
/// opposite orders.
///
/// Defaults to [`Transform::IDENTITY`] (not glam's zero), so
/// `Transform::default()` placed on an entity is a no-op rather than a
/// degenerate scale-0 matrix.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    pub translation: Vec3,
    /// A normalized quaternion, as required by glam's rotation matrices.
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Transform {
    /// The no-op placement: no translation, no rotation, unit scale.
    pub const IDENTITY: Self = Self {
        translation: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        scale: Vec3::ONE,
    };

    /// Translation only, identity rotation, unit scale.
    pub const fn from_translation(translation: Vec3) -> Self {
        Self {
            translation,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        }
    }

    /// Rotation only, at the origin, unit scale. `rotation` must be normalized.
    pub const fn from_rotation(rotation: Quat) -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation,
            scale: Vec3::ONE,
        }
    }

    /// Scale only, at the origin, identity rotation.
    pub const fn from_scale(scale: Vec3) -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale,
        }
    }

    /// Scale → rotate → translate, as a 4x4 matrix (`Mat4` is column-major
    /// and maps as `M * v`, matching glam's convention).
    pub fn to_matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }

    /// Apply the full TRS to a point (w = 1, so translation counts).
    pub fn transform_point(&self, point: Vec3) -> Vec3 {
        self.to_matrix().transform_point3(point)
    }

    /// Apply rotation and scale only — a direction, not a position.
    pub fn transform_vector(&self, vector: Vec3) -> Vec3 {
        self.to_matrix().transform_vector3(vector)
    }

    /// The inverse as a matrix, applying inverse translation, then inverse
    /// rotation, then inverse scale.
    ///
    /// Nonuniform scale and rotation do not generally commute, so the inverse
    /// cannot in general be represented by another scale-rotate-translate
    /// [`Transform`]. Use [`Mat4::transform_point3`] or
    /// [`Mat4::transform_vector3`] on the returned matrix.
    ///
    /// Returns `None` for zero scale, nonfinite fields, a non-normalized
    /// rotation, or when computing the inverse produces nonfinite values.
    /// Negative scale is supported.
    ///
    /// ```
    /// use oriera_common::{Transform, Vec3};
    ///
    /// let transform = Transform::from_translation(Vec3::new(1.0, 2.0, 3.0));
    /// let inverse = transform.inverse().unwrap();
    /// assert_eq!(inverse.transform_point3(transform.translation), Vec3::ZERO);
    /// ```
    pub fn inverse(&self) -> Option<Mat4> {
        if !self.translation.is_finite()
            || !self.rotation.is_finite()
            || !self.rotation.is_normalized()
            || !self.scale.is_finite()
            || self.scale.x == 0.0
            || self.scale.y == 0.0
            || self.scale.z == 0.0
        {
            return None;
        }
        let inv_scale = self.scale.recip();
        if !inv_scale.is_finite() {
            return None;
        }
        // (T R S)^-1 = S^-1 R^-1 T^-1. Keep these factors in matrix form;
        // putting them back in a Transform would reverse S^-1 and R^-1.
        let inverse = Mat4::from_scale(inv_scale)
            * Mat4::from_quat(self.rotation.conjugate())
            * Mat4::from_translation(-self.translation);
        inverse.is_finite().then_some(inverse)
    }
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    fn assert_close(actual: Vec3, expected: Vec3) {
        assert!(
            (actual - expected).length() <= 1e-5,
            "expected {expected:?}, got {actual:?}"
        );
    }

    fn assert_matrix_close(actual: Mat4, expected: Mat4) {
        assert!(
            actual.abs_diff_eq(expected, 1e-5),
            "expected {expected:?}, got {actual:?}"
        );
    }

    #[test]
    fn vec3_basic_ops_are_glam() {
        let a = Vec3::new(1.0, 0.0, 0.0);
        let b = Vec3::new(0.0, 1.0, 0.0);
        assert_eq!(a + b, Vec3::new(1.0, 1.0, 0.0));
        assert_eq!(a - b, Vec3::new(1.0, -1.0, 0.0));
        assert_eq!(a.dot(b), 0.0);
        assert_eq!(a.cross(b), Vec3::new(0.0, 0.0, 1.0));
        assert!((Vec3::new(3.0, 4.0, 0.0).normalize().length() - 1.0).abs() <= 1e-6);
    }

    #[test]
    fn identity_matrix_is_identity() {
        let m = Transform::IDENTITY.to_matrix();
        assert_eq!(m, Mat4::IDENTITY);
        assert_close(
            Transform::IDENTITY.transform_point(Vec3::new(1.0, 2.0, 3.0)),
            Vec3::new(1.0, 2.0, 3.0),
        );
    }

    #[test]
    fn translation_moves_points_but_not_directions() {
        let t = Transform::from_translation(Vec3::new(1.0, 2.0, 3.0));
        assert_close(t.transform_point(Vec3::ZERO), Vec3::new(1.0, 2.0, 3.0));
        assert_close(
            t.transform_point(Vec3::new(1.0, 0.0, 0.0)),
            Vec3::new(2.0, 2.0, 3.0),
        );
        // A direction carries no position, so translation must not touch it.
        assert_close(t.transform_vector(Vec3::X), Vec3::X);
    }

    #[test]
    fn quarter_turn_about_z_maps_x_to_y() {
        let t = Transform::from_rotation(Quat::from_rotation_z(FRAC_PI_2));
        assert_close(t.transform_point(Vec3::X), Vec3::Y);
    }

    #[test]
    fn scale_applies_before_rotation_and_translation() {
        let t = Transform {
            translation: Vec3::new(0.0, 0.0, 5.0),
            rotation: Quat::from_rotation_z(FRAC_PI_2),
            scale: Vec3::new(2.0, 3.0, 4.0),
        };
        // scale (1,1,1) -> (2,3,4), rotate -> (-3,2,4), translate -> +5 z.
        assert_close(t.transform_point(Vec3::ONE), Vec3::new(-3.0, 2.0, 9.0));
    }

    #[test]
    fn inverse_round_trips_a_point() {
        let t = Transform {
            translation: Vec3::new(3.0, -1.0, 0.5),
            rotation: Quat::from_rotation_z(FRAC_PI_2),
            scale: Vec3::new(2.0, 2.0, 2.0),
        };
        let inv = t.inverse().expect("non-singular transform");
        let world = t.transform_point(Vec3::new(1.0, 0.5, 0.25));
        assert_close(inv.transform_point3(world), Vec3::new(1.0, 0.5, 0.25));
    }

    #[test]
    fn inverse_nonuniform_scale_round_trips_points_and_vectors() {
        for scale in [Vec3::new(2.0, 3.0, 4.0), Vec3::new(-2.0, 3.0, -4.0)] {
            let t = Transform {
                translation: Vec3::new(3.0, -1.0, 0.5),
                rotation: Quat::from_axis_angle(Vec3::new(1.0, 2.0, 3.0).normalize(), 0.7),
                scale,
            };
            let inv = t.inverse().expect("non-singular transform");
            for point in [
                Vec3::ZERO,
                Vec3::new(1.0, 0.5, 0.25),
                Vec3::new(-2.0, 3.0, 1.0),
            ] {
                assert_close(inv.transform_point3(t.transform_point(point)), point);
                assert_close(t.transform_point(inv.transform_point3(point)), point);
            }
            for vector in [Vec3::X, Vec3::Y, Vec3::Z, Vec3::new(1.0, -2.0, 3.0)] {
                assert_close(inv.transform_vector3(t.transform_vector(vector)), vector);
                assert_close(t.transform_vector(inv.transform_vector3(vector)), vector);
            }
        }
    }

    #[test]
    fn inverse_nonuniform_scale_is_a_two_sided_matrix_inverse() {
        let t = Transform {
            translation: Vec3::new(3.0, -1.0, 0.5),
            rotation: Quat::from_axis_angle(Vec3::new(1.0, 2.0, 3.0).normalize(), 0.7),
            scale: Vec3::new(2.0, 3.0, -4.0),
        };
        let matrix = t.to_matrix();
        let inv = t.inverse().expect("non-singular transform");
        assert_matrix_close(inv * matrix, Mat4::IDENTITY);
        assert_matrix_close(matrix * inv, Mat4::IDENTITY);
        assert_matrix_close(inv, matrix.inverse());
    }

    #[test]
    fn inverse_refuses_singular_scale() {
        for zero in [0.0, -0.0] {
            for scale in [
                Vec3::new(zero, 1.0, 1.0),
                Vec3::new(1.0, zero, 1.0),
                Vec3::new(1.0, 1.0, zero),
            ] {
                assert!(Transform::from_scale(scale).inverse().is_none());
            }
        }
    }

    #[test]
    fn inverse_handles_identity_and_single_components() {
        for t in [
            Transform::IDENTITY,
            Transform::from_translation(Vec3::new(3.0, -1.0, 0.5)),
            Transform::from_rotation(Quat::from_rotation_z(0.7)),
            Transform::from_scale(Vec3::new(-2.0, 3.0, 4.0)),
        ] {
            let inv = t.inverse().expect("non-singular transform");
            assert_matrix_close(inv * t.to_matrix(), Mat4::IDENTITY);
            assert_matrix_close(t.to_matrix() * inv, Mat4::IDENTITY);
        }
    }

    #[test]
    fn inverse_handles_finite_scales_with_extreme_determinants() {
        // A generic f32 determinant underflows/overflows for these scales,
        // but the individual scale reciprocals and inverse matrices are finite.
        for scale in [1e-20, 1e20] {
            let t = Transform {
                scale: Vec3::splat(scale),
                rotation: Quat::from_rotation_z(0.7),
                ..Transform::IDENTITY
            };
            let inv = t.inverse().expect("finite inverse");
            assert!(inv.is_finite());
            assert_matrix_close(inv * t.to_matrix(), Mat4::IDENTITY);
            assert_matrix_close(t.to_matrix() * inv, Mat4::IDENTITY);
        }
    }

    #[test]
    fn inverse_refuses_nonfinite_fields() {
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            for vector in [
                Vec3::new(value, 1.0, 1.0),
                Vec3::new(1.0, value, 1.0),
                Vec3::new(1.0, 1.0, value),
            ] {
                assert!(Transform::from_translation(vector).inverse().is_none());
                assert!(Transform::from_scale(vector).inverse().is_none());
            }
            for rotation in [
                Quat::from_xyzw(value, 0.0, 0.0, 1.0),
                Quat::from_xyzw(0.0, value, 0.0, 1.0),
                Quat::from_xyzw(0.0, 0.0, value, 1.0),
                Quat::from_xyzw(0.0, 0.0, 0.0, value),
            ] {
                assert!(Transform::from_rotation(rotation).inverse().is_none());
            }
        }
    }

    #[test]
    fn inverse_refuses_non_normalized_rotation() {
        for rotation in [
            Quat::from_xyzw(0.0, 0.0, 0.0, 0.0),
            Quat::from_xyzw(0.0, 0.0, 0.0, 2.0),
        ] {
            assert!(Transform::from_rotation(rotation).inverse().is_none());
        }
    }

    #[test]
    fn inverse_refuses_nonfinite_results() {
        // This nonzero scale has a reciprocal outside the finite f32 range.
        let tiny_scale = Vec3::new(f32::from_bits(1), 1.0, 1.0);
        assert!(Transform::from_scale(tiny_scale).inverse().is_none());

        // Finite inputs and reciprocals can still overflow the translation.
        let t = Transform {
            translation: Vec3::splat(f32::MAX),
            scale: Vec3::splat(0.5),
            ..Transform::IDENTITY
        };
        assert!(t.inverse().is_none());
    }

    #[test]
    fn transform_serde_round_trip() {
        let t = Transform {
            translation: Vec3::new(1.0, 2.0, 3.0),
            rotation: Quat::from_rotation_z(FRAC_PI_2),
            scale: Vec3::splat(2.0),
        };
        let json = serde_json::to_string(&t).expect("serialize");
        let back: Transform = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(t, back);
    }
}
