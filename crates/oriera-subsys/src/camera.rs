//! Top-down RGB camera for the embodied bench (feature `cam`).
//!
//! A deterministic software rasterizer over the table-top scene: table
//! slab, goal disc, cube, and the arm skeleton drawn as thick segments.
//! It is an honest *overhead camera* — the modality tabletop policies
//! actually consume (ManiSkill-style state images) — not a pretend
//! photoreal render; the wgpu path stays the M1 upgrade. Same input ⇒
//! same PNG bytes, pinned by the env-level determinism tests.

use crate::rigid3d::PROP_HALF;
use crate::Scene3D;

/// Orthographic top-down view: world `center`/`extent` (x, z in
/// metres) mapped onto a `w×h` RGB image (+x → right, +z → down).
#[derive(Debug, Clone)]
pub struct CameraView {
    pub center: [f32; 2],
    pub extent: [f32; 2],
    pub w: u32,
    pub h: u32,
}

impl Default for CameraView {
    fn default() -> Self {
        // Frames the whole table with margin — the workspace the
        // policies act in.
        Self {
            center: [0.32, 0.0],
            extent: [0.92, 0.92],
            w: 168,
            h: 168,
        }
    }
}

impl CameraView {
    fn px(&self, x: f32, z: f32) -> (i32, i32) {
        let px = ((x - (self.center[0] - self.extent[0] / 2.0)) / self.extent[0] * self.w as f32)
            .round() as i32;
        let py = ((z - (self.center[1] - self.extent[1] / 2.0)) / self.extent[1] * self.h as f32)
            .round() as i32;
        (px, py)
    }
}

struct Canvas {
    rgb: Vec<u8>,
    w: u32,
    h: u32,
}

impl Canvas {
    fn new(w: u32, h: u32, bg: [u8; 3]) -> Self {
        Self {
            rgb: vec![0; (w * h * 3) as usize],
            w,
            h,
        }
        .with_bg(bg)
    }
    fn with_bg(mut self, bg: [u8; 3]) -> Self {
        for i in 0..(self.w * self.h) as usize {
            self.rgb[i * 3..i * 3 + 3].copy_from_slice(&bg);
        }
        self
    }
    fn put(&mut self, x: i32, y: i32, c: [u8; 3]) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        let i = (y as u32 * self.w + x as u32) as usize * 3;
        self.rgb[i..i + 3].copy_from_slice(&c);
    }
    fn fill_rect(&mut self, cx: i32, cy: i32, hw: i32, hh: i32, c: [u8; 3]) {
        for y in (cy - hh)..=(cy + hh) {
            for x in (cx - hw)..=(cx + hw) {
                self.put(x, y, c);
            }
        }
    }
    fn disc(&mut self, cx: i32, cy: i32, r: i32, c: [u8; 3]) {
        for y in (cy - r)..=(cy + r) {
            for x in (cx - r)..=(cx + r) {
                if (x - cx).pow(2) + (y - cy).pow(2) <= r * r {
                    self.put(x, y, c);
                }
            }
        }
    }
    fn line(&mut self, a: (i32, i32), b: (i32, i32), r: i32, c: [u8; 3]) {
        // Bresenham + radius stamp — integer math only, deterministic.
        let (mut x, mut y) = a;
        let (dx, dy) = (b.0 - x, b.1 - y);
        let (sx, sy) = (dx.signum(), dy.signum());
        let (adx, ady) = (dx.abs(), dy.abs());
        let mut err = adx - ady;
        loop {
            self.disc(x, y, r, c);
            if x == b.0 && y == b.1 {
                break;
            }
            let e2 = 2 * err;
            if e2 > -ady {
                err -= ady;
                x += sx;
            }
            if e2 < adx {
                err += adx;
                y += sy;
            }
        }
    }
}

/// Render one top-down frame as raw RGB.
pub fn render_top_down(
    view: &CameraView,
    scene: &Scene3D,
    chain: &[[f32; 3]],
    cube: [f32; 3],
    goal: [f32; 3],
) -> Vec<u8> {
    let mut cv = Canvas::new(view.w, view.h, [24, 27, 36]);
    let (tc, th) = scene
        .table
        .unwrap_or(([0.32, 0.25, 0.0], [0.28, 0.25, 0.32]));
    let (tcx, tcy) = view.px(tc[0], tc[2]);
    let (thx, thy) = view.px(th[0], th[2]);
    cv.fill_rect(
        tcx,
        tcy,
        (thx - view.px(0.0, 0.0).0).abs(),
        (thy - view.px(0.0, 0.0).1).abs(),
        [56, 68, 88],
    );
    let (gx, gy) = view.px(goal[0], goal[2]);
    let gr = (0.045 / view.extent[0] * view.w as f32).round() as i32;
    cv.disc(gx, gy, gr, [64, 200, 132]);
    // arm skeleton
    let blue = [91, 141, 239];
    for w in pairs(chain) {
        let a = view.px(w[0][0], w[0][2]);
        let b = view.px(w[1][0], w[1][2]);
        cv.line(a, b, 2, blue);
    }
    let (cxp, cyp) = view.px(cube[0], cube[2]);
    let half = (PROP_HALF / view.extent[0] * view.w as f32).round() as i32;
    cv.fill_rect(cxp, cyp, half, half, [255, 206, 110]);
    cv.rgb
}

fn pairs(chain: &[[f32; 3]]) -> Vec<[[f32; 3]; 2]> {
    chain.windows(2).map(|w| [w[0], w[1]]).collect()
}

/// Encode raw RGB as PNG bytes (deterministic for a fixed crate build).
pub fn encode_png(rgb: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(|e| format!("png header: {e}"))?;
        writer
            .write_image_data(rgb)
            .map_err(|e| format!("png data: {e}"))?;
    }
    Ok(out)
}

/// Render + encode in one call (the env's per-tick camera path).
pub fn capture_png(
    view: &CameraView,
    scene: &Scene3D,
    chain: &[[f32; 3]],
    cube: [f32; 3],
    goal: [f32; 3],
) -> Result<Vec<u8>, String> {
    let rgb = render_top_down(view, scene, chain, cube, goal);
    encode_png(&rgb, view.w, view.h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_scene_same_png_bytes() {
        let view = CameraView::default();
        let scene = Scene3D::default();
        let chain = vec![
            [0.0, 0.62, 0.0],
            [0.2, 0.6, 0.0],
            [0.35, 0.55, 0.1],
            [0.42, 0.5, 0.1],
        ];
        let a = capture_png(&view, &scene, &chain, [0.3, 0.52, 0.1], [0.5, 0.5, -0.1]).unwrap();
        let b = capture_png(&view, &scene, &chain, [0.3, 0.52, 0.1], [0.5, 0.5, -0.1]).unwrap();
        assert_eq!(a, b, "same scene ⇒ bit-identical PNG");
        let moved = capture_png(&view, &scene, &chain, [0.3, 0.52, 0.1], [0.5, 0.5, 0.0]).unwrap();
        assert_ne!(a, moved, "one pixel of change ⇒ different PNG");
    }

    #[test]
    fn pixels_land_inside_the_frame() {
        let view = CameraView::default();
        let (px, py) = view.px(0.32, 0.0);
        assert!((0..view.w as i32).contains(&px));
        assert!((0..view.h as i32).contains(&py));
    }
}
