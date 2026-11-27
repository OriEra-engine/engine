//! L4 程序化散布:植被等实例在地形表面的确定性放置——真实场景批次 WP5。
//!
//! [`scatter`] 在 [`Terrain`](crate::terrain::Terrain) 上铺抖动网格
//! (jittered grid):每格一个候选点,位置抖动、均匀缩放、绕 y 朝向由
//! 每点独立的 LCG 派生;地表法线与 up 夹角超过 `max_slope_deg` 的点被
//! 剔除;输出 `oriera_common::Transform`。纯 std、wasm32 可编译;同种子
//! 同 spec 两次散布逐位一致。
//!
//! **契约**
//! - 确定性:种子推进沿用仓库 LCG 约定(常数同 `embodied_env::Lcg`:
//!   `state = state·6364136223846793005 + 1442695040888963407`,取高 31
//!   位映射到 [0,1]);每点初始状态由 (seed, 格点坐标) 经 FNV-1a 64 变体
//!   字节混合派生(不用 `DefaultHasher`),剔除与遍历都不携带跨点状态。
//!   每点按固定次序抽 `jitter_x → jitter_z → scale → yaw`,顺序是契约的
//!   一部分。
//! - 地表访问:只经 terrain 公开的 `sample_bilinear` / `normal_at` 取
//!   高度与法线,不读其内部字段(模块间单向、仅公开接口)。
//! - 输出用 #7 `oriera_common` 词汇:`rotation = Quat::from_rotation_y`
//!   (单位四元数,满足 `Transform` 对归一化的要求),`scale =
//!   Vec3::splat`(均匀),平移 y = 该点地形高度,与
//!   `sample_bilinear(x, z)` 逐位相等。
//! - 坐标 y 向上;输出顺序 = 格点行优先(z 行在外、x 列在内),剔除的
//!   点直接不入列,不打乱其余点的顺序。
//!
//! **目标数与边界规则**(±25% 容差的口径)
//! - 目标数 = `density × 面积 / 100`(`density` = 每 100 单位² 的期望
//!   个数);网格间距 `stride = √(100/density)`。
//! - **只计完整格**:nx = floor(extent_x / stride)、nz =
//!   floor(extent_z / stride),不完整的边缘格不计入——这是实际数与
//!   目标数之间最大的系统偏差(少不足一格/轴),容差覆盖它;坡度剔除
//!   在此之上再减。
//! - 抖动 ±½·stride 后点理论上仍在场内,仍钳制到 [0, extent] 以吸收
//!   浮点舍入。
//!
//! **诚实边界**
//! - 抖动网格是蓝噪声/Poisson-disk 的廉价替代:相邻抖动点可以任意贴近,
//!   最小点距无硬保证,不要当泊松采样用。
//! - 剔除只看坡度:没有密度图、没有高度带(树线)、没有排斥/吸引物,
//!   生态规则留给上层过滤。
//! - `density` ≤ 0、非有限,或 stride 退化 ≤ 0 时返回空(按"无散布"
//!   处理,不 panic);density 很大时点数按 1/stride² 增长,本模块不做
//!   上限拦截,规模由调用方把握。
//! - 缩放均匀、朝向仅绕 y:立木垂直于 y 轴而非坡面,无各向异性缩放;
//!   min/max 缩放里的非有限值不拦截,会原样进入输出。

use crate::terrain::Terrain;
// Quat 是 glam 词汇,crate 根只 re-export Transform/Vec3,四元数走
// oriera_common::math(见 oriera-common/src/math.rs 的 `pub use glam`)。
use oriera_common::math::Quat;
use oriera_common::{Transform, Vec3};

/// 散布规格。字段私有,经 [`ScatterSpec::new`] 构造(min/max 缩放自动
/// 纠序),只读访问走字段(同 crate)或复制规格。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScatterSpec {
    /// 每 100 单位² 的目标个数(目标,非保证;见模块文档边界规则)。
    density: f32,
    /// 散布种子;同种子同 spec → 布局逐位一致。
    seed: u64,
    /// 均匀缩放下限。
    min_scale: f32,
    /// 均匀缩放上限。
    max_scale: f32,
    /// 允许的地表法线与 up 最大夹角(度);超过即剔除。
    max_slope_deg: f32,
}

impl ScatterSpec {
    /// 构造散布规格。
    ///
    /// - `density`:每 100 单位² 目标个数;运行时 ≤ 0 / 非有限 → 空输出。
    /// - `min_scale` / `max_scale`:均匀缩放范围;min > max 时两者对调。
    /// - `max_slope_deg`:坡度上限(度)。≥ 90° 接受全部(法线与 up 夹角
    ///   恒 < 90°),极小值可清场——梯度剔除测试即由此构造。
    pub fn new(
        density: f32,
        seed: u64,
        min_scale: f32,
        max_scale: f32,
        max_slope_deg: f32,
    ) -> Self {
        let (min_scale, max_scale) = if min_scale <= max_scale {
            (min_scale, max_scale)
        } else {
            (max_scale, min_scale)
        };
        Self {
            density,
            seed,
            min_scale,
            max_scale,
            max_slope_deg,
        }
    }
}

/// 仓库 LCG 约定(常数同 `embodied_env::scene::Lcg`):种子推进是确定性
/// 契约的一部分。
struct Lcg(u64);

impl Lcg {
    /// 推进一步并返回 [lo, hi] 内的均匀采样。
    fn next_f32(&mut self, lo: f32, hi: f32) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let f = (self.0 >> 33) as f32 / (u32::MAX >> 1) as f32;
        lo + f * (hi - lo)
    }
}

/// 从 (seed, 格点坐标) 派生每点 LCG 初始状态——FNV-1a 64 变体逐字节
/// 混合,不用 `DefaultHasher`。同种子同格点 → 同序列。
fn point_seed(seed: u64, i: u32, j: u32) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for v in [seed, u64::from(i), u64::from(j)] {
        for b in v.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

/// 在地形表面做抖动网格散布。
///
/// 格点基位置 = (i + ½)·stride;每点按固定次序抽 `jitter_x → jitter_z
/// → scale → yaw`,抖动后钳入 [0, extent];坡度剔除:地形法线与 up 夹角
/// 超过 `max_slope_deg` 的点跳过;点高度 = `terrain.sample_bilinear(x, z)`
/// (逐位)。顺序 = 格点行优先。
pub fn scatter(t: &Terrain, spec: &ScatterSpec) -> Vec<Transform> {
    let stride = (100.0 / spec.density).sqrt();
    // NaN / 0 / 负 stride 一律按"无散布"返回空,不 panic(density ≤ 0
    // 或 NaN 时 stride 为 NaN;density = +inf 时 stride 为 0)。
    if !stride.is_finite() || stride <= 0.0 {
        // density ≤ 0 / NaN → stride 为 NaN;density=+inf → stride 为 0。
        // 都按"无散布"返回空,不 panic。
        return Vec::new();
    }
    let [ex, ez] = t.extent();
    // 只计完整格(边界规则,见模块文档);floor 后仍须有限,极端 density
    // 的规模风险由调用方把握。
    let nx = (ex / stride).floor() as u32;
    let nz = (ez / stride).floor() as u32;
    let mut out = Vec::new();
    for j in 0..nz {
        for i in 0..nx {
            let mut rng = Lcg(point_seed(spec.seed, i, j));
            let x = (i as f32 + 0.5) * stride + rng.next_f32(-0.5, 0.5) * stride;
            let z = (j as f32 + 0.5) * stride + rng.next_f32(-0.5, 0.5) * stride;
            let scale = rng.next_f32(spec.min_scale, spec.max_scale);
            let yaw = rng.next_f32(0.0, std::f32::consts::TAU);
            let (x, z) = (x.clamp(0.0, ex), z.clamp(0.0, ez));
            // 坡度剔除:cos(夹角) = n·up = n[1](法线单位长、y 分量恒正),
            // acos 后与阈值比较,阈值 ≥ 90° 时自然全收。
            let n = t.normal_at(x, z);
            let slope = n[1].clamp(-1.0, 1.0).acos();
            if slope > spec.max_slope_deg.to_radians() {
                continue;
            }
            let y = t.sample_bilinear(x, z);
            out.push(Transform {
                translation: Vec3::new(x, y, z),
                rotation: Quat::from_rotation_y(yaw),
                scale: Vec3::splat(scale),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terrain::TerrainSpec;

    /// 和缓地形:低起伏 + 粗格距,坡度上限全开时所有格点都能通过。
    fn flat_terrain(seed: u64) -> Terrain {
        Terrain::generate(TerrainSpec::new(65, 65, 2.0, seed, 3, 2.0))
    }

    fn scatter_spec(seed: u64, density: f32) -> ScatterSpec {
        ScatterSpec::new(density, seed, 0.8, 1.2, 90.0)
    }

    #[test]
    fn same_seed_scatter_is_bit_identical() {
        let t = flat_terrain(9);
        let a = scatter(&t, &scatter_spec(42, 1.0));
        let b = scatter(&t, &scatter_spec(42, 1.0));
        assert_eq!(a.len(), b.len());
        for (p, q) in a.iter().zip(&b) {
            assert_eq!(p, q, "同种子必须逐位复现布局");
        }
    }

    #[test]
    fn different_seed_layout_differs() {
        let t = flat_terrain(9);
        let a = scatter(&t, &scatter_spec(1, 1.0));
        let b = scatter(&t, &scatter_spec(2, 1.0));
        assert_ne!(a, b, "不同种子必须给出不同布局");
    }

    #[test]
    fn count_follows_floor_grid_rule_and_density_target() {
        let t = flat_terrain(9);
        let [ex, ez] = t.extent();
        for density in [0.5, 1.0, 4.0] {
            let out = scatter(&t, &scatter_spec(7, density));
            let stride = (100.0 / density).sqrt();
            // 边界规则:只计完整格(floor),不完整边缘格丢弃
            let nx = (ex / stride).floor() as u32;
            let nz = (ez / stride).floor() as u32;
            assert_eq!(out.len() as u32, nx * nz, "density {density} 格数不符");
            // ±25% 容差对目标数
            let target = density * ex * ez / 100.0;
            assert!(
                (out.len() as f32) >= 0.75 * target && (out.len() as f32) <= 1.25 * target,
                "density {density}: 实际 {} 离目标 {target} 超 ±25%",
                out.len()
            );
        }
    }

    #[test]
    fn y_equals_sample_bilinear_exactly() {
        let t = flat_terrain(3);
        let out = scatter(&t, &scatter_spec(5, 1.5));
        assert!(!out.is_empty());
        for p in &out {
            let y = t.sample_bilinear(p.translation.x, p.translation.z);
            assert_eq!(
                p.translation.y.to_bits(),
                y.to_bits(),
                "y 必须逐位等于采样值"
            );
        }
    }

    #[test]
    fn slope_filter_rejects_steep_terrain() {
        // 刻意构造陡峭地形:大 height_scale + 细格距 + 满八度 → fBm 梯度陡
        let t = Terrain::generate(TerrainSpec::new(33, 33, 1.0, 77, 8, 60.0));
        let lenient = ScatterSpec::new(4.0, 7, 1.0, 1.0, 89.0);
        let strict = ScatterSpec::new(4.0, 7, 1.0, 1.0, 2.0);
        let loose = scatter(&t, &lenient);
        let kept = scatter(&t, &strict);
        assert!(!loose.is_empty(), "坡度上限 89° 不应剔点");
        assert!(
            kept.is_empty(),
            "坡度 > 2° 应全部剔除,实际保留 {} 点",
            kept.len()
        );
        // 用同一地形公开接口复核宽松规格的每个保留点确实在阈值内
        for p in &loose {
            let n = t.normal_at(p.translation.x, p.translation.z);
            assert!(n[1].acos() <= 89f32.to_radians());
        }
    }

    #[test]
    fn transforms_carry_scale_yaw_and_in_bounds_positions() {
        let t = flat_terrain(4);
        let (min_s, max_s) = (0.8, 1.2);
        let out = scatter(&t, &ScatterSpec::new(1.0, 21, min_s, max_s, 90.0));
        assert!(!out.is_empty());
        let [ex, ez] = t.extent();
        for p in &out {
            // 均匀缩放,落在 [min, max]
            assert!((min_s..=max_s).contains(&p.scale.x), "scale {}", p.scale.x);
            assert_eq!(p.scale.x, p.scale.y);
            assert_eq!(p.scale.y, p.scale.z);
            // 绕 y 的单位四元数:旋转 x 轴不会离开 xz 平面
            assert!(p.rotation.is_normalized());
            let turned = p.rotation * Vec3::X;
            assert!(
                turned.y.abs() < 1e-6,
                "yaw 旋转把 x 轴转出水平面: {turned:?}"
            );
            // 位置在场内(钳制吸收舍入)
            assert!(
                (0.0..=ex).contains(&p.translation.x),
                "x {}",
                p.translation.x
            );
            assert!(
                (0.0..=ez).contains(&p.translation.z),
                "z {}",
                p.translation.z
            );
        }
    }

    #[test]
    fn degenerate_density_yields_empty() {
        let t = flat_terrain(2);
        assert!(scatter(&t, &ScatterSpec::new(0.0, 1, 1.0, 1.0, 90.0)).is_empty());
        assert!(scatter(&t, &ScatterSpec::new(-1.0, 1, 1.0, 1.0, 90.0)).is_empty());
        assert!(scatter(&t, &ScatterSpec::new(f32::NAN, 1, 1.0, 1.0, 90.0)).is_empty());
        assert!(scatter(&t, &ScatterSpec::new(f32::INFINITY, 1, 1.0, 1.0, 90.0)).is_empty());
        // 单点地形:extent 0,无完整格
        let point = Terrain::generate(TerrainSpec::new(1, 1, 1.0, 3, 1, 5.0));
        assert!(scatter(&point, &scatter_spec(1, 1.0)).is_empty());
    }
}
