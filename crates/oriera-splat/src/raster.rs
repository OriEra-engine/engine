//! CPU 参考光栅:前到背透射率累积(Kerbl et al. 2023 参考实现的合成规则)。
//!
//! 每个 splat 只在 3σ 椭圆的轴对齐包围框(`extent`)内求值——这是本内核的**规范求值域**,
//! M1 GPU 路径以同尺寸四边形覆盖同一域。逐像素:
//! `power = -½(a·dx² + c·dy²) - b·dx·dy`,`α = min(0.99, opacity·e^power)`;
//! α < 1/255 跳过;若 `T·(1-α) < 1e-4`,该像素提前终止(本 splat 不计入);
//! 否则 `C += color·α·T`,`T ← T·(1-α)`。最终 `rgb = C + T·background`。
//! 与后到前 over 合成代数等价,早停带来的差 ≤ 1e-4 < 1/255。
//!
//! 这是**参考/金样路径**:复杂度 ∝ 覆盖像素总数,受 [`RasterBudget`] 约束,
//! 超预算返回 [`RasterError::OverBudget`]——绝不静默降级。

use crate::camera::SplatCamera;
use crate::project::{project, ALPHA_MIN};
use crate::sort::depth_order;
use oriera_asset::ply::SplatCloud;
use std::fmt;

/// 单像素 α 上限(参考实现取 0.99,保证透射率不会一步归零)。
pub const ALPHA_MAX: f32 = 0.99;
/// 透射率低于此值即视为饱和,像素停止合成。
pub const T_MIN: f32 = 1e-4;

/// CPU 参考路径的输入预算:splat 数与像素数分别封顶。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RasterBudget {
    pub max_splats: usize,
    pub max_pixels: u64,
}

impl Default for RasterBudget {
    /// 5 千 splat、1280×720:CPU 参考路径的保守默认(实测见 docs/STABILITY.md)。
    fn default() -> Self {
        Self {
            max_splats: 5_000,
            max_pixels: 1280 * 720,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RasterOptions {
    /// 背景色(线性 RGB);未被覆盖的部分按透射率透出。
    pub background: [f32; 3],
    pub budget: RasterBudget,
}

impl Default for RasterOptions {
    fn default() -> Self {
        Self {
            background: [0.0; 3],
            budget: RasterBudget::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RasterError {
    /// 相机宽或高为 0。
    EmptyViewport,
    /// 输入超出 CPU 参考路径预算——显式失败,绝不静默降级。
    OverBudget {
        splats: usize,
        pixels: u64,
        budget: RasterBudget,
    },
}

impl fmt::Display for RasterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RasterError::EmptyViewport => write!(f, "camera viewport is empty"),
            RasterError::OverBudget {
                splats,
                pixels,
                budget,
            } => write!(
                f,
                "CPU splat reference over budget: {splats} splats / {pixels} px \
                 (limits {} splats / {} px); raise RasterBudget explicitly",
                budget.max_splats, budget.max_pixels
            ),
        }
    }
}

impl std::error::Error for RasterError {}

/// 光栅结果:RGBA8,行主序自上而下;RGB 为非预乘颜色、已在背景上合成;
/// A = 覆盖率 `1 - T`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplatImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl SplatImage {
    /// 像素 (x, y) 的 RGBA。
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let at = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.rgba[at],
            self.rgba[at + 1],
            self.rgba[at + 2],
            self.rgba[at + 3],
        ]
    }

    /// 可审计帧指纹:FNV-1a(种子与 `World::state_hash` 同为 ASCII "oriera"),
    /// 依次混入宽、高、像素字节(按 8 字节小端字、末字补零)。
    pub fn fingerprint(&self) -> u64 {
        let mut h: u64 = 0x0000_6f72_6965_7261;
        mix(&mut h, u64::from(self.width));
        mix(&mut h, u64::from(self.height));
        for chunk in self.rgba.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            mix(&mut h, u64::from_le_bytes(word));
        }
        h
    }
}

fn mix(h: &mut u64, word: u64) {
    *h ^= word;
    *h = h.wrapping_mul(0x100000001b3);
}

/// 渲染一帧。超预算或空视口返回错误;否则输出逐字节确定。
pub fn rasterize(
    cloud: &SplatCloud,
    camera: &SplatCamera,
    opts: &RasterOptions,
) -> Result<SplatImage, RasterError> {
    let (width, height) = (camera.width, camera.height);
    if width == 0 || height == 0 {
        return Err(RasterError::EmptyViewport);
    }
    let pixels = u64::from(width) * u64::from(height);
    let budget = opts.budget;
    if cloud.len() > budget.max_splats || pixels > budget.max_pixels {
        return Err(RasterError::OverBudget {
            splats: cloud.len(),
            pixels,
            budget,
        });
    }

    let projected = project(cloud, camera);
    let row = width as usize;
    let len = row * height as usize;
    let mut color = vec![[0f32; 3]; len];
    let mut trans = vec![1f32; len];
    let mut done = vec![false; len];
    for slot in depth_order(&projected) {
        let p = &projected[slot as usize];
        let Some((x0, x1)) = span(p.mean[0], p.extent[0], width) else {
            continue;
        };
        let Some((y0, y1)) = span(p.mean[1], p.extent[1], height) else {
            continue;
        };
        for py in y0..=y1 {
            let dy = py as f32 + 0.5 - p.mean[1];
            for px in x0..=x1 {
                let at = py * row + px;
                if done[at] {
                    continue;
                }
                let dx = px as f32 + 0.5 - p.mean[0];
                let power =
                    -0.5 * (p.conic[0] * dx * dx + p.conic[2] * dy * dy) - p.conic[1] * dx * dy;
                if power > 0.0 {
                    continue;
                }
                let alpha = (p.opacity * power.exp()).min(ALPHA_MAX);
                if alpha < ALPHA_MIN {
                    continue;
                }
                let t = trans[at];
                let next = t * (1.0 - alpha);
                if next < T_MIN {
                    done[at] = true;
                    continue;
                }
                let weight = alpha * t;
                for (acc, c) in color[at].iter_mut().zip(p.color) {
                    *acc += c * weight;
                }
                trans[at] = next;
            }
        }
    }

    let mut rgba = Vec::with_capacity(len * 4);
    for (c, &t) in color.iter().zip(&trans) {
        for (v, bg) in c.iter().zip(opts.background) {
            rgba.push(quantize(v + t * bg));
        }
        rgba.push(quantize(1.0 - t));
    }
    Ok(SplatImage {
        width,
        height,
        rgba,
    })
}

/// 像素中心落在 `[mean - half, mean + half]` 内的下标闭区间,钳到画面;完全在画外则 None。
fn span(mean: f32, half: f32, extent: u32) -> Option<(usize, usize)> {
    let lo = (mean - half - 0.5).ceil();
    let hi = (mean + half - 0.5).floor();
    let last = extent as f32 - 1.0;
    if hi < 0.0 || lo > last || lo > hi {
        return None;
    }
    Some((lo.max(0.0) as usize, hi.min(last) as usize))
}

fn quantize(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene;
    use oriera_asset::ply::SH_C0;

    const IDENTITY: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

    /// 33×33,相机在 z=-4 看原点:原点投影到像素 (16, 16) 的中心。
    fn camera() -> SplatCamera {
        SplatCamera::look_at([0.0, 0.0, -4.0], [0.0; 3], [0.0, 1.0, 0.0], 60.0, 33, 33)
    }

    fn push(c: &mut SplatCloud, pos: [f32; 3], scale: f32, rgb: [f32; 3], opacity: f32) {
        c.positions.push(pos);
        c.scales.push([scale; 3]);
        c.rotations.push(IDENTITY);
        c.opacities.push(opacity);
        c.sh_dc.push(rgb.map(|v| (v - 0.5) / SH_C0));
    }

    #[test]
    fn empty_cloud_renders_the_background() {
        let opts = RasterOptions {
            background: [0.2, 0.4, 0.6],
            ..Default::default()
        };
        let img = rasterize(&SplatCloud::default(), &camera(), &opts).unwrap();
        assert_eq!(img.rgba.len(), 33 * 33 * 4);
        assert!(img
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| *p == [51, 102, 153, 0]));
    }

    #[test]
    fn single_splat_center_pixel_matches_closed_form() {
        let mut c = SplatCloud::default();
        push(&mut c, [0.0; 3], 0.4, [0.9, 0.2, 0.1], 0.6);
        let bg = [0.0, 0.0, 0.5];
        let opts = RasterOptions {
            background: bg,
            ..Default::default()
        };
        let img = rasterize(&c, &camera(), &opts).unwrap();
        // 中心像素 dx = dy = 0 → power = 0 → α = opacity;rgb = α·color + (1-α)·bg。
        let col = c.base_color(0);
        let t = 1.0 - 0.6f32;
        let want = [
            quantize(col[0] * 0.6 + t * bg[0]),
            quantize(col[1] * 0.6 + t * bg[1]),
            quantize(col[2] * 0.6 + t * bg[2]),
            quantize(0.6),
        ];
        assert_eq!(img.pixel(16, 16), want);
        // 足迹对称:左右、上下对称像素相同。
        assert_eq!(img.pixel(14, 16), img.pixel(18, 16));
        assert_eq!(img.pixel(16, 13), img.pixel(16, 19));
    }

    #[test]
    fn nearer_splat_occludes_the_farther_one() {
        // 远处的绿放在下标 0,近处的红放在下标 1:顺序必须来自深度而不是下标。
        let mut c = SplatCloud::default();
        push(&mut c, [0.0, 0.0, 1.0], 0.5, [0.0, 1.0, 0.0], 0.99);
        push(&mut c, [0.0, 0.0, -1.0], 0.5, [1.0, 0.0, 0.0], 0.99);
        let px = rasterize(&c, &camera(), &RasterOptions::default())
            .unwrap()
            .pixel(16, 16);
        assert!(px[0] >= 250, "red should dominate: {px:?}");
        assert_eq!(px[1], 0, "far green must be hidden: {px:?}");
    }

    #[test]
    fn over_budget_is_a_visible_error_not_a_degraded_image() {
        let mut c = SplatCloud::default();
        push(&mut c, [0.0; 3], 0.3, [1.0; 3], 0.5);
        push(&mut c, [0.1, 0.0, 0.0], 0.3, [1.0; 3], 0.5);
        let tight = RasterOptions {
            budget: RasterBudget {
                max_splats: 1,
                max_pixels: 1 << 20,
            },
            ..Default::default()
        };
        assert!(matches!(
            rasterize(&c, &camera(), &tight),
            Err(RasterError::OverBudget { splats: 2, .. })
        ));
        let few_pixels = RasterOptions {
            budget: RasterBudget {
                max_splats: 10,
                max_pixels: 100,
            },
            ..Default::default()
        };
        assert!(matches!(
            rasterize(&c, &camera(), &few_pixels),
            Err(RasterError::OverBudget { pixels: 1089, .. })
        ));
    }

    #[test]
    fn empty_viewport_is_rejected() {
        let mut cam = camera();
        cam.width = 0;
        assert_eq!(
            rasterize(&SplatCloud::default(), &cam, &RasterOptions::default()),
            Err(RasterError::EmptyViewport)
        );
    }

    #[test]
    fn output_is_byte_identical_across_runs_and_input_permutations() {
        let cloud = scene::petal_ring(400, 11);
        let cam = scene::petal_camera(96, 64);
        let opts = RasterOptions {
            background: scene::BRAND_BACKGROUND,
            ..Default::default()
        };
        let a = rasterize(&cloud, &cam, &opts).unwrap();
        let b = rasterize(&cloud, &cam, &opts).unwrap();
        assert_eq!(a, b);

        let mut reversed = cloud.clone();
        reversed.positions.reverse();
        reversed.scales.reverse();
        reversed.rotations.reverse();
        reversed.opacities.reverse();
        reversed.sh_dc.reverse();
        let r = rasterize(&reversed, &cam, &opts).unwrap();
        assert_eq!(a.rgba, r.rgba);
        assert_eq!(a.fingerprint(), r.fingerprint());
    }

    #[test]
    fn fingerprint_covers_dimensions_and_every_byte() {
        let base = SplatImage {
            width: 2,
            height: 1,
            rgba: vec![9; 8],
        };
        let transposed = SplatImage {
            width: 1,
            height: 2,
            ..base.clone()
        };
        let mut flipped = base.clone();
        flipped.rgba[7] ^= 1;
        assert_ne!(base.fingerprint(), transposed.fingerprint());
        assert_ne!(base.fingerprint(), flipped.fingerprint());
        assert_eq!(base.fingerprint(), base.clone().fingerprint());
    }
}
