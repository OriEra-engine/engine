//! EWA 投影:3D 高斯 → 屏幕 2D 高斯(Zwicker et al. 2001 的局部仿射近似;
//! 常数与剔除规则对齐 Kerbl et al. 2023 参考实现)。
//!
//! 剔除即 compaction(Spark/Radix 的管线模式):相机后方或近平面内、协方差退化、
//! 包围框完全在画外、不透明度低于 1/255 的 splat 在排序之前就被丢弃。
//!
//! 包围框取 3σ 椭圆的轴对齐外接矩形(半宽 3√Σxx、半高 3√Σyy),而不是参考实现按
//! 最大特征值取的外接正方形:3σ 内的像素一个不少,细长 splat 却省掉大片角落。

use crate::camera::{dot, SplatCamera};
use oriera_asset::ply::SplatCloud;

/// 视空间深度不超过此值的 splat 直接剔除(参考实现取 0.2)。
pub const NEAR_Z: f32 = 0.2;
/// 2D 协方差对角线的低通膨胀(像素²):抗混叠,并保证足迹至少约一像素。
pub const LOW_PASS: f32 = 0.3;
/// 低于此 α 的贡献不合成(8 位输出不可见)。
pub const ALPHA_MIN: f32 = 1.0 / 255.0;

/// 投影后幸存的 2D 高斯;光栅只读这一层。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProjectedSplat {
    /// 在原 [`SplatCloud`] 中的下标(排序破平局用,保证全序确定)。
    pub index: u32,
    /// 视空间深度 z(> [`NEAR_Z`])。
    pub depth: f32,
    /// 投影中心(像素,连续坐标)。
    pub mean: [f32; 2],
    /// 2D 协方差的逆(conic)上三角 `[a, b, c]`:`power = -½(a·dx² + c·dy²) - b·dx·dy`。
    pub conic: [f32; 3],
    /// 3σ 椭圆轴对齐包围框的半宽、半高(像素,已向上取整)——光栅的规范求值域。
    pub extent: [f32; 2],
    /// 线性 RGB(DC 项,只钳下限)。
    pub color: [f32; 3],
    pub opacity: f32,
}

/// 投影整朵云,返回剔除后的幸存者(保持原下标升序)。
pub fn project(cloud: &SplatCloud, camera: &SplatCamera) -> Vec<ProjectedSplat> {
    let (tan_x, tan_y) = camera.tan_half_fov();
    let limits = (1.3 * tan_x, 1.3 * tan_y);
    (0..cloud.len())
        .filter_map(|i| project_one(cloud, i, camera, limits))
        .collect()
}

fn project_one(
    cloud: &SplatCloud,
    i: usize,
    cam: &SplatCamera,
    (lim_x, lim_y): (f32, f32),
) -> Option<ProjectedSplat> {
    let opacity = cloud.opacities[i];
    if opacity.is_nan() || opacity < ALPHA_MIN {
        return None;
    }
    let t = cam.to_camera(cloud.positions[i]);
    let z = t[2];
    if z.is_nan() || z <= NEAR_Z {
        return None;
    }

    // Σ = M·Mᵀ,M = R·S(R 由单位四元数得出,S = diag(线性尺度))。
    let r = quat_to_mat(cloud.rotations[i]);
    let s = cloud.scales[i];
    let m = r.map(|row| [row[0] * s[0], row[1] * s[1], row[2] * s[2]]);
    // 相机系:W·M;再乘 Jacobian 得 T = J·W·M(2×3),2D 协方差 = T·Tᵀ(恒半正定)。
    let wm = mat_mul(&cam.rotation, &m);
    // 参考实现把 x/z、y/z 钳到 ±1.3·tan(fov/2):视野边缘的仿射近似否则会爆炸。
    let tx = (t[0] / z).clamp(-lim_x, lim_x) * z;
    let ty = (t[1] / z).clamp(-lim_y, lim_y) * z;
    let j0 = [cam.fx / z, 0.0, -cam.fx * tx / (z * z)];
    let j1 = [0.0, cam.fy / z, -cam.fy * ty / (z * z)];
    let t0 = row_times(j0, &wm);
    let t1 = row_times(j1, &wm);
    let a = dot(t0, t0) + LOW_PASS;
    let b = dot(t0, t1);
    let c = dot(t1, t1) + LOW_PASS;

    let det = a * c - b * b;
    if det.is_nan() || det <= 0.0 {
        return None;
    }
    let inv = 1.0 / det;
    let extent = [(3.0 * a.sqrt()).ceil(), (3.0 * c.sqrt()).ceil()];
    let mean = [cam.fx * t[0] / z + cam.cx, cam.fy * t[1] / z + cam.cy];
    if mean[0] + extent[0] < 0.0
        || mean[1] + extent[1] < 0.0
        || mean[0] - extent[0] > cam.width as f32
        || mean[1] - extent[1] > cam.height as f32
    {
        return None;
    }
    Some(ProjectedSplat {
        index: i as u32,
        depth: z,
        mean,
        conic: [c * inv, -b * inv, a * inv],
        extent,
        color: cloud.base_color(i),
        opacity,
    })
}

/// 单位四元数 `[x, y, z, w]` → 旋转矩阵(行主序)。
fn quat_to_mat([x, y, z, w]: [f32; 4]) -> [[f32; 3]; 3] {
    [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y - w * z),
            2.0 * (x * z + w * y),
        ],
        [
            2.0 * (x * y + w * z),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z - w * x),
        ],
        [
            2.0 * (x * z - w * y),
            2.0 * (y * z + w * x),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ]
}

fn mat_mul(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    a.map(|row| row_times(row, b))
}

/// 行向量 × 3×3 矩阵。
fn row_times(v: [f32; 3], m: &[[f32; 3]; 3]) -> [f32; 3] {
    [
        v[0] * m[0][0] + v[1] * m[1][0] + v[2] * m[2][0],
        v[0] * m[0][1] + v[1] * m[1][1] + v[2] * m[2][1],
        v[0] * m[0][2] + v[1] * m[1][2] + v[2] * m[2][2],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

    /// (位置, 线性尺度, xyzw 四元数, 不透明度)
    type Spec = ([f32; 3], [f32; 3], [f32; 4], f32);

    fn cloud(splats: &[Spec]) -> SplatCloud {
        let mut c = SplatCloud::default();
        for &(pos, scale, rot, opacity) in splats {
            c.positions.push(pos);
            c.scales.push(scale);
            c.rotations.push(rot);
            c.opacities.push(opacity);
            c.sh_dc.push([0.0; 3]);
        }
        c
    }

    /// 相机在 z=-4 看向原点:原点深度 4,投影到画面中心。
    fn camera() -> SplatCamera {
        SplatCamera::look_at([0.0, 0.0, -4.0], [0.0; 3], [0.0, 1.0, 0.0], 60.0, 33, 33)
    }

    #[test]
    fn isotropic_splat_matches_closed_form() {
        let s = 0.5;
        let p = project(&cloud(&[([0.0; 3], [s; 3], IDENTITY, 0.8)]), &camera());
        assert_eq!(p.len(), 1);
        let p = p[0];
        let cam = camera();
        let a = (cam.fx * s / 4.0).powi(2) + LOW_PASS;
        assert!((p.depth - 4.0).abs() < 1e-5);
        assert!((p.mean[0] - 16.5).abs() < 1e-4 && (p.mean[1] - 16.5).abs() < 1e-4);
        assert!((p.conic[0] - 1.0 / a).abs() < 1e-6 && (p.conic[2] - 1.0 / a).abs() < 1e-6);
        assert!(p.conic[1].abs() < 1e-7);
        // 各向同性:包围框是正方形,半边 = 3σ
        assert_eq!(p.extent, [(3.0 * a.sqrt()).ceil(); 2]);
    }

    #[test]
    fn behind_camera_and_inside_near_plane_are_culled() {
        let c = cloud(&[
            ([0.0, 0.0, -5.0], [0.1; 3], IDENTITY, 0.9), // 相机身后
            ([0.0, 0.0, -3.9], [0.1; 3], IDENTITY, 0.9), // 深度 0.1 < NEAR_Z
            ([0.0, 0.0, 1.0], [0.1; 3], IDENTITY, 0.9),  // 深度 5,可见
        ]);
        let p = project(&c, &camera());
        assert_eq!(p.iter().map(|s| s.index).collect::<Vec<_>>(), [2]);
    }

    #[test]
    fn offscreen_and_invisible_splats_are_compacted_away() {
        let c = cloud(&[
            ([500.0, 0.0, 0.0], [0.1; 3], IDENTITY, 0.9), // 画外
            ([0.0; 3], [0.1; 3], IDENTITY, 0.001),        // α 上限 < 1/255
            ([0.0; 3], [0.1; 3], IDENTITY, 0.5),
        ]);
        let p = project(&c, &camera());
        assert_eq!(p.iter().map(|s| s.index).collect::<Vec<_>>(), [2]);
    }

    #[test]
    fn zero_scale_splat_stays_finite_thanks_to_low_pass() {
        let p = project(&cloud(&[([0.0; 3], [0.0; 3], IDENTITY, 0.9)]), &camera());
        assert_eq!(p.len(), 1);
        let p = p[0];
        assert!(p.conic.iter().all(|v| v.is_finite()));
        assert!((p.conic[0] - 1.0 / LOW_PASS).abs() < 1e-4);
        assert_eq!(p.extent, [(3.0 * LOW_PASS.sqrt()).ceil(); 2]);
    }

    #[test]
    fn rotation_tilts_the_footprint_the_right_way() {
        // 长轴沿局部 x,绕世界 +Z 转 +45°:世界里长轴指向 (+x, +y)。相机在 z=-4 看 +Z,
        // 画面 u = 世界 -x、v = 世界 -y,长轴投影到 (-,-) 方向 → u、v 正相关 → conic b < 0。
        // R 与 Rᵀ 弄反时长轴转到 -45°,b 的符号随之翻转。
        let half = std::f32::consts::FRAC_PI_8;
        let tilt = [0.0, 0.0, half.sin(), half.cos()];
        let p = project(&cloud(&[([0.0; 3], [1.0, 0.1, 0.1], tilt, 0.9)]), &camera());
        assert!(p[0].conic[1] < 0.0, "conic {:?}", p[0].conic);

        // 不转:长轴水平 → 水平方差大 → conic[0] 小于 conic[2],包围框横长竖短
        let p = project(
            &cloud(&[([0.0; 3], [1.0, 0.1, 0.1], IDENTITY, 0.9)]),
            &camera(),
        );
        assert!(p[0].conic[0] < p[0].conic[2]);
        assert!(p[0].extent[0] > 3.0 * p[0].extent[1], "{:?}", p[0].extent);
    }
}
