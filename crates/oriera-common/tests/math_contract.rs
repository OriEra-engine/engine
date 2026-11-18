//! L0 数学词汇的公共契约测试（#7）。
//!
//! `src/math.rs` 的单元测试用 `super::*`，证明的是模块内部自洽；这里从
//! crate 外部按消费方视角钉住对上层真正承诺的东西：re-export 的入口、
//! TRS 组合顺序、逆变换的往返与拒绝条件、以及 serde 的线上格式（场景
//! spec / run manifest 直接吃这层 JSON，格式一变就是跨层静默错位）。

use std::f32::consts::{FRAC_PI_2, PI};

use oriera_common::math::{Mat3, Mat4, Quat, Vec2, Vec3};
use oriera_common::{Transform, Vec3 as RootVec3};

const EPS: f32 = 1e-4;

fn assert_vec3_close(actual: Vec3, expected: Vec3) {
    assert!(
        (actual - expected).length() <= EPS,
        "expected {expected:?}, got {actual:?}"
    );
}

fn assert_mat4_close(actual: Mat4, expected: Mat4) {
    assert!(
        actual.abs_diff_eq(expected, EPS),
        "expected {expected:?}, got {actual:?}"
    );
}

fn assert_copy<T: Copy>() {}
fn assert_send_sync<T: Send + Sync + 'static>() {}

/// 确定性伪随机：不引 proptest 也能扫一批非平凡 TRS 组合。
struct Lcg(u64);

impl Lcg {
    fn next_unit(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 40) as f32) / ((1u64 << 24) as f32)
    }

    fn next_in(&mut self, low: f32, high: f32) -> f32 {
        low + (high - low) * self.next_unit()
    }

    fn next_sign(&mut self) -> f32 {
        if self.next_unit() < 0.5 {
            -1.0
        } else {
            1.0
        }
    }
}

#[test]
fn crate_root_reexports_are_the_same_types_as_math() {
    // #7 的唯一外部入口契约：`oriera_common::Vec3` 必须就是 glam 的 Vec3，
    // 不能是包了一层的新类型（否则 L3→L5→L6 传递时要转换）。
    let _: RootVec3 = Vec3::ZERO;

    // math 模块承诺的五个 glam 名词都可用，且不做 fork。
    let _: Vec2 = Vec2::new(1.0, 2.0);
    let _: Mat3 = Mat3::IDENTITY;
    let _: Mat4 = Mat4::IDENTITY;
    let _: Quat = Quat::IDENTITY;

    assert_copy::<Transform>();
    assert_send_sync::<Transform>();
    assert_copy::<Vec3>();
    assert_send_sync::<Vec3>();
}

#[test]
fn default_is_identity_not_a_degenerate_zero_scale() {
    assert_eq!(Transform::default(), Transform::IDENTITY);
    assert_eq!(Transform::default().scale, Vec3::ONE);
    assert_eq!(Transform::default().translation, Vec3::ZERO);
    assert_eq!(Transform::default().to_matrix(), Mat4::IDENTITY);
}

#[test]
fn constructors_only_set_the_component_they_name() {
    let translation = Transform::from_translation(Vec3::new(1.0, -2.0, 0.5));
    assert_eq!(translation.rotation, Quat::IDENTITY);
    assert_eq!(translation.scale, Vec3::ONE);

    let rotation = Transform::from_rotation(Quat::from_rotation_z(FRAC_PI_2));
    assert_eq!(rotation.translation, Vec3::ZERO);
    assert_eq!(rotation.scale, Vec3::ONE);

    let scale = Transform::from_scale(Vec3::new(2.0, 3.0, 4.0));
    assert_eq!(scale.translation, Vec3::ZERO);
    assert_eq!(scale.rotation, Quat::IDENTITY);
}

#[test]
fn to_matrix_is_scale_then_rotation_then_translation() {
    let t = Transform {
        translation: Vec3::new(0.0, 0.0, 5.0),
        rotation: Quat::from_rotation_z(FRAC_PI_2),
        scale: Vec3::new(2.0, 3.0, 4.0),
    };

    assert_mat4_close(
        t.to_matrix(),
        Mat4::from_scale_rotation_translation(t.scale, t.rotation, t.translation),
    );

    // 顺序可观测：先缩 (1,1,1)→(2,3,4)，再转 90°→(-3,2,4)，最后 +5z。
    // 反过来「先转后缩」会得到 (-2,3,4)+(0,0,5)，两者不是同一个矩阵。
    assert_vec3_close(t.transform_point(Vec3::ONE), Vec3::new(-3.0, 2.0, 9.0));
}

#[test]
fn transform_vector_applies_scale_and_rotation_but_ignores_translation() {
    let t = Transform {
        translation: Vec3::new(7.0, -3.0, 2.0),
        rotation: Quat::from_rotation_z(FRAC_PI_2),
        scale: Vec3::new(2.0, 3.0, 4.0),
    };

    // X 先被 x 轴缩放成 (2,0,0)，再绕 z 转 90° 落在 y 轴上；
    // 平移对方向不生效，否则「朝向」会跟着物体世界坐标漂移。
    assert_vec3_close(t.transform_vector(Vec3::X), Vec3::new(0.0, 2.0, 0.0));
}

#[test]
fn inverse_round_trips_a_deterministic_sweep_of_transforms() {
    let mut rng = Lcg(0x0DDB_A11E_5EED);
    for case in 0..64 {
        let scale = Vec3::new(
            rng.next_sign() * rng.next_in(0.25, 4.0),
            rng.next_sign() * rng.next_in(0.25, 4.0),
            rng.next_sign() * rng.next_in(0.25, 4.0),
        );
        let axis = Vec3::new(
            rng.next_in(-1.0, 1.0),
            rng.next_in(-1.0, 1.0),
            rng.next_in(-1.0, 1.0),
        );
        // 轴长不够就别归一化——采到近零向量时换一个固定非零轴。
        let axis = if axis.length() < 1e-3 {
            Vec3::new(1.0, 2.0, 3.0)
        } else {
            axis
        };
        let rotation = Quat::from_axis_angle(axis.normalize(), rng.next_in(-PI, PI));
        let translation = Vec3::new(
            rng.next_in(-5.0, 5.0),
            rng.next_in(-5.0, 5.0),
            rng.next_in(-5.0, 5.0),
        );
        let t = Transform {
            translation,
            rotation,
            scale,
        };

        let inv = t
            .inverse()
            .unwrap_or_else(|| panic!("case {case} should be invertible: {t:?}"));

        for point in [
            Vec3::ZERO,
            Vec3::new(1.0, 0.5, 0.25),
            Vec3::new(-2.0, 3.0, 1.0),
        ] {
            assert_vec3_close(inv.transform_point3(t.transform_point(point)), point);
            assert_vec3_close(t.transform_point(inv.transform_point3(point)), point);
        }
        for vector in [Vec3::X, Vec3::Y, Vec3::Z, Vec3::new(1.0, -2.0, 3.0)] {
            assert_vec3_close(inv.transform_vector3(t.transform_vector(vector)), vector);
            assert_vec3_close(t.transform_vector(inv.transform_vector3(vector)), vector);
        }

        // 非均匀缩放与旋转不可交换，逆必须是一个真正的双侧矩阵逆。
        let matrix = t.to_matrix();
        assert_mat4_close(inv * matrix, Mat4::IDENTITY);
        assert_mat4_close(matrix * inv, Mat4::IDENTITY);
    }
}

#[test]
fn inverse_refuses_singular_and_nonfinite_inputs() {
    for zero in [0.0, -0.0] {
        for scale in [
            Vec3::new(zero, 1.0, 1.0),
            Vec3::new(1.0, zero, 1.0),
            Vec3::new(1.0, 1.0, zero),
        ] {
            assert!(
                Transform::from_scale(scale).inverse().is_none(),
                "zero scale {scale:?} has no inverse"
            );
        }
    }

    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert!(Transform::from_translation(Vec3::splat(value))
            .inverse()
            .is_none());
        assert!(Transform::from_scale(Vec3::splat(value))
            .inverse()
            .is_none());
        assert!(
            Transform::from_rotation(Quat::from_xyzw(value, 0.0, 0.0, 1.0))
                .inverse()
                .is_none()
        );
    }

    // 零四元数与超长四元数都不满足 glam 的旋转前提。
    for rotation in [
        Quat::from_xyzw(0.0, 0.0, 0.0, 0.0),
        Quat::from_xyzw(0.0, 0.0, 0.0, 2.0),
    ] {
        assert!(Transform::from_rotation(rotation).inverse().is_none());
    }

    // 有限输入也能算出非有限结果：最小次正规数的倒数直接溢出。
    assert!(
        Transform::from_scale(Vec3::new(f32::from_bits(1), 1.0, 1.0))
            .inverse()
            .is_none()
    );
}

#[test]
fn transform_serializes_as_a_flat_trs_triple() {
    let t = Transform {
        translation: Vec3::new(1.0, 2.0, 3.0),
        rotation: Quat::from_rotation_z(FRAC_PI_2),
        scale: Vec3::splat(2.0),
    };
    let json = serde_json::to_value(t).expect("serialize");
    let object = json.as_object().expect("Transform must be a JSON object");

    // 字段名是跨层契约：场景 spec 与 run manifest 按这三个名字读。
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["rotation", "scale", "translation"]);

    // glam 的 serde 形式：Vec3 是 [x, y, z]，Quat 是 [x, y, z, w]。
    assert_eq!(json["translation"], serde_json::json!([1.0, 2.0, 3.0]));
    assert_eq!(json["scale"], serde_json::json!([2.0, 2.0, 2.0]));
    let rotation = json["rotation"].as_array().expect("Quat must be an array");
    assert_eq!(rotation.len(), 4);

    let back: Transform = serde_json::from_value(json).expect("deserialize");
    assert_eq!(back, t);
}
