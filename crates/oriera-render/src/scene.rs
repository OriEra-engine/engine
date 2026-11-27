//! 场景词汇(feature `scene-vocab`)——真实场景批次里剔除与合批共用的
//! 最小数据类型。只放"两边都要认得"的三样东西:包围球、场景实例、合批。
//!
//! 约定
//! - 全部世界空间、y 向上(与 [`crate::camera`] 同一坐标系);
//! - 矩阵沿用 [`crate::camera::Mat4`](行主序、列向量),不引入第二种矩阵;
//! - `mesh` / `material` 是调用方命名空间里的 id(L4 资产句柄,或 L5 GPU
//!   句柄的 CPU 侧影子),L5 不解释其含义,只按相等性分组;
//! - [`DrawBatch`] 是 GPU 侧 `DrawList`(feature `render-wgpu`)的 CPU
//!   前身:逐批、按 `instances` 下标顺序 `push` 即得同一绘制序列。
//!
//! 诚实边界
//! - 包围球是粗边界:变换后的半径取模型矩阵 3×3 部分的最大行长,对带
//!   剪切或非均匀缩放的矩阵是保守上界(偏大,不会偏小);
//! - 没有实例层级(父子变换)、没有运动模糊包围、没有材质排序键。

use crate::camera::{transform, Mat4};

/// 世界空间包围球。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub center: [f32; 3],
    pub radius: f32,
}

impl Bounds {
    /// 半径为 0 的球,位于原点——"没有几何"的占位。
    pub const EMPTY: Self = Self {
        center: [0.0; 3],
        radius: 0.0,
    };

    pub const fn new(center: [f32; 3], radius: f32) -> Self {
        Self { center, radius }
    }

    /// 经模型矩阵变换后的包围球:球心按点变换,半径乘 3×3 部分的最大
    /// 行长(保守上界——平移所在的第四列不参与)。
    pub fn transformed(&self, model: &Mat4) -> Self {
        let [x, y, z, w] = transform(model, self.center);
        let inv_w = if w.abs() > f32::MIN_POSITIVE {
            1.0 / w
        } else {
            1.0
        };
        Self {
            center: [x * inv_w, y * inv_w, z * inv_w],
            radius: self.radius * max_row_norm(model),
        }
    }
}

/// 模型矩阵 3×3 部分的最大行范数:球半径经该矩阵放大的保守系数。
fn max_row_norm(model: &Mat4) -> f32 {
    let mut worst = 0.0_f32;
    for row in model.iter().take(3) {
        let norm = (row[0] * row[0] + row[1] * row[1] + row[2] * row[2]).sqrt();
        worst = worst.max(norm);
    }
    worst
}

/// 一个网格的一次摆放:合批与剔除的共同输入单位。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SceneInstance {
    pub mesh: u32,
    pub material: u32,
    /// 模型矩阵(局部 → 世界),行主序。
    pub model: Mat4,
    /// 世界空间包围球。
    pub bounds: Bounds,
}

impl SceneInstance {
    /// 由局部包围球推导世界包围球。
    pub fn new(mesh: u32, material: u32, model: Mat4, local_bounds: Bounds) -> Self {
        Self {
            mesh,
            material,
            bounds: local_bounds.transformed(&model),
            model,
        }
    }
}

/// 一个合批:同网格、同材质的实例下标列表,顺序即绘制顺序。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrawBatch {
    pub mesh: u32,
    pub material: u32,
    /// 指向输入实例列表的下标。
    pub instances: Vec<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translation(x: f32, y: f32, z: f32) -> Mat4 {
        let mut m = identity();
        m[0][3] = x;
        m[1][3] = y;
        m[2][3] = z;
        m
    }

    fn scale(sx: f32, sy: f32, sz: f32) -> Mat4 {
        let mut m = identity();
        m[0][0] = sx;
        m[1][1] = sy;
        m[2][2] = sz;
        m
    }

    fn identity() -> Mat4 {
        let mut m = [[0.0; 4]; 4];
        for (i, row) in m.iter_mut().enumerate() {
            row[i] = 1.0;
        }
        m
    }

    #[test]
    fn translation_moves_center_keeps_radius() {
        let b = Bounds::new([1.0, 2.0, 3.0], 0.5).transformed(&translation(10.0, -4.0, 2.0));
        assert_eq!(b.center, [11.0, -2.0, 5.0]);
        assert_eq!(b.radius, 0.5);
    }

    #[test]
    fn non_uniform_scale_takes_the_largest_axis() {
        let b = Bounds::new([0.0; 3], 1.0).transformed(&scale(2.0, 5.0, 3.0));
        assert_eq!(b.radius, 5.0);
        assert_eq!(b.center, [0.0; 3]);
    }

    #[test]
    fn transformed_sphere_covers_the_transformed_local_sphere() {
        // 剪切 + 非均匀缩放:最坏情况下仍必须包住局部球面上的采样点。
        let mut m = scale(1.5, 0.5, 2.0);
        m[0][1] = 0.8;
        m[2][0] = -0.6;
        m[0][3] = 3.0;
        let local = Bounds::new([1.0, -1.0, 0.5], 0.75);
        let world = local.transformed(&m);
        for (dx, dy, dz) in [
            (1.0, 0.0, 0.0),
            (0.0, 1.0, 0.0),
            (0.0, 0.0, 1.0),
            (-0.5773503, -0.5773503, -0.5773503),
        ] {
            let p = [
                local.center[0] + dx * local.radius,
                local.center[1] + dy * local.radius,
                local.center[2] + dz * local.radius,
            ];
            let [px, py, pz, _] = transform(&m, p);
            let d = ((px - world.center[0]).powi(2)
                + (py - world.center[1]).powi(2)
                + (pz - world.center[2]).powi(2))
            .sqrt();
            assert!(
                d <= world.radius * 1.0001,
                "sample point escaped the sphere: d={d} r={}",
                world.radius
            );
        }
    }

    #[test]
    fn instance_derives_world_bounds_from_local() {
        let inst = SceneInstance::new(7, 9, translation(1.0, 0.0, 0.0), Bounds::new([0.0; 3], 2.0));
        assert_eq!((inst.mesh, inst.material), (7, 9));
        assert_eq!(inst.bounds, Bounds::new([1.0, 0.0, 0.0], 2.0));
    }
}
