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

    /// Rotation only, at the origin, unit scale.
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

    /// The inverse placement, or `None` when a scale component is zero
    /// (that matrix is singular and has no inverse to return).
    ///
    /// Cheaper and more accurate than inverting the 4x4: the rotation
    /// inverts as the conjugate quaternion, the scale as a reciprocal.
    pub fn inverse(&self) -> Option<Self> {
        if self.scale.x == 0.0 || self.scale.y == 0.0 || self.scale.z == 0.0 {
            return None;
        }
        let inv_scale = Vec3::new(1.0 / self.scale.x, 1.0 / self.scale.y, 1.0 / self.scale.z);
        let inv_rotation = self.rotation.conjugate();
        // (T R S)^-1 = S^-1 R^-1 T^-1: the inverse translation is the
        // conjugated rotation applied to the negated, inverse-scaled
        // translation.
        let inv_translation = inv_rotation * (-self.translation * inv_scale);
        Some(Self {
            translation: inv_translation,
            rotation: inv_rotation,
            scale: inv_scale,
        })
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
            scale: Vec3::splat(2.0),
        };
        // scale (1,0,0) -> (2,0,0), rotate -> (0,2,0), translate -> +5 z.
        assert_close(t.transform_point(Vec3::X), Vec3::new(0.0, 2.0, 5.0));
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
        assert_close(inv.transform_point(world), Vec3::new(1.0, 0.5, 0.25));
    }

    #[test]
    fn inverse_refuses_singular_scale() {
        let t = Transform::from_scale(Vec3::new(1.0, 0.0, 1.0));
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
