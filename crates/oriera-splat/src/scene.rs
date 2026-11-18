//! 确定性程序化场景(演示/基准/金样用,**非稳定契约**)。
//!
//! 三圈切向排布的扁长椭球「花瓣」+ 中心小亮核,配色取 Q∞ 品牌色。各向异性是有意为之:
//! 3DGS 与粒子点云的视觉分界,正在于椭球的朝向与长短轴。

use crate::camera::SplatCamera;
use oriera_asset::ply::{SplatCloud, SH_C0};
use std::f32::consts::{FRAC_PI_2, TAU};

/// 三圈花瓣的颜色:#3ee6ff / #ffc861 / #ff6b2b。
const PALETTE: [[f32; 3]; 3] = [
    [0.243, 0.902, 1.0],
    [1.0, 0.784, 0.380],
    [1.0, 0.420, 0.169],
];

/// Q∞ 深色底 #070b16。
pub const BRAND_BACKGROUND: [f32; 3] = [0.027, 0.043, 0.086];

/// `count` 个 splat 的花瓣环;同一 `(count, seed)` 永远产出同一朵云。
pub fn petal_ring(count: usize, seed: u64) -> SplatCloud {
    let mut rng = Lcg(seed);
    let core = count / 10;
    let mut cloud = SplatCloud {
        comments: vec![format!(
            "comment oriera-splat petal_ring count={count} seed={seed}"
        )],
        ..SplatCloud::default()
    };
    for i in 0..count {
        let (pos, scale, rot, rgb, opacity) = if i < core {
            // 中心亮核:小而近各向同性。
            let r = 0.22 * rng.unit().sqrt();
            let a = TAU * rng.unit();
            let s = 0.02 + 0.02 * rng.unit();
            (
                [r * a.cos(), 0.04 * (rng.unit() - 0.5), r * a.sin()],
                [s; 3],
                [0.0, 0.0, 0.0, 1.0],
                [1.0, 0.95, 0.85],
                0.5 + 0.4 * rng.unit(),
            )
        } else {
            // 花瓣:长轴(局部 x)沿圆环切向,局部 y 最薄(躺平),局部 z 为瓣宽。
            let ring = (i - core) % 3;
            let radius = 0.9 + 0.6 * ring as f32 + 0.08 * (rng.unit() - 0.5);
            let theta = TAU * rng.unit();
            let y = 0.15 * (3.0 * theta).sin() + 0.04 * (rng.unit() - 0.5);
            let length = 0.12 + 0.1 * rng.unit();
            let half = -(theta + FRAC_PI_2) * 0.5;
            let shade = 0.75 + 0.35 * rng.unit();
            (
                [radius * theta.cos(), y, radius * theta.sin()],
                [length, 0.01, 0.035],
                [0.0, half.sin(), 0.0, half.cos()],
                PALETTE[ring].map(|c| c * shade),
                0.55 + 0.4 * rng.unit(),
            )
        };
        cloud.positions.push(pos);
        cloud.scales.push(scale);
        cloud.rotations.push(rot);
        cloud.opacities.push(opacity);
        cloud.sh_dc.push(rgb.map(|c| (c - 0.5) / SH_C0));
    }
    cloud
}

/// 与 [`petal_ring`] 配套的斜俯视相机。
pub fn petal_camera(width: u32, height: u32) -> SplatCamera {
    SplatCamera::look_at(
        [0.0, 2.4, -4.2],
        [0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        45.0,
        width,
        height,
    )
}

/// 64 位 LCG(Knuth MMIX 常数),取高 24 位得 [0, 1) 均匀浮点。
struct Lcg(u64);

impl Lcg {
    fn unit(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_cloud_and_colors_round_trip_through_dc() {
        let a = petal_ring(300, 5);
        assert_eq!(a, petal_ring(300, 5));
        assert_ne!(a, petal_ring(300, 6));
        assert_eq!(a.len(), 300);
        for (i, q) in a.rotations.iter().enumerate() {
            let len: f32 = q.iter().map(|v| v * v).sum();
            assert!((len - 1.0).abs() < 1e-5, "splat {i} quaternion not unit");
        }
        // 核心第一颗的颜色由 DC 往返还原。
        let col = a.base_color(0);
        assert!((col[0] - 1.0).abs() < 1e-5 && (col[2] - 0.85).abs() < 1e-5);
    }
}
