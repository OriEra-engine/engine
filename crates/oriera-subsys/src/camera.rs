//! Top-down RGB camera for the embodied bench (feature `cam`).
//!
//! Two renderers share this module:
//!
//! * the legacy **orthographic top-down** view
//!   ([`render_top_down`], [`CameraView`]) — the default `--cam`
//!   path, byte-frozen since M0a;
//! * the **pinhole perspective** pipeline (M3, PLAN §四 M3 / §9.1
//!   U-5): [`PinholeCam`] (extrinsics pos/look_at/up + intrinsics
//!   fx/fy/cx/cy + Brown–Conrady distortion) × [`SceneView`]
//!   primitives → RGB + millimetre depth ([`render_pinhole`]).
//!
//! Same-input ⇒ same-bytes determinism holds for both: f32 arithmetic
//! in fixed loops, no data-dependent iteration order, distortion as a
//! pure post-map.
//!
//! Both are honest *state images*, not photoreal renders: flat-shaded
//! analytic primitives (cuboids/capsules/cylinders/discs), one
//! head-light directional, no textures. The texture-level wgpu path is
//! explicitly **not** planned (PLAN §八 "RTX 渲染 不做"); real-machine
//! transfer treats these as calibrated synthetic cameras instead.

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

// ===========================================================================
// Pinhole perspective pipeline (M3, PLAN §四 M3 / §9.1 U-5)
// ===========================================================================
//
// Extrinsics come from pos/look_at/up (or, env-side, from the end-effector
// frame tick by tick); intrinsics are fx/fy/cx/cy pixels; distortion is the
// Brown–Conrady 4-coefficient model (radial k1,k2 + tangential p1,p2).
// Rendering order: near-clip in camera space → ideal-pinhole projection →
// z-buffered flat-shaded rasterization → distortion as a pure post-map
// (inverse-mapped nearest-neighbour resample). Because the post-map runs
// only when a coefficient is non-zero, the zero-distortion path is
// byte-identical to "no distortion" — and `project()` applies the same
// forward map, so a projected world point lands exactly on the pixel its
// surface rasterized into (the ≤2 px calibration gate in
// `examples/embodied_cam_capture.rs`).
//
// Depth is the camera-frame forward distance in **millimetres**, u16, 0 =
// no return (the sensor convention; the scene sits well under 65 m).

/// Brown–Conrady lens distortion: radial `k1`/`k2` + tangential `p1`/`p2`.
/// All-zero (= the spec default) means an ideal pinhole.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Distortion {
    pub k1: f32,
    pub k2: f32,
    pub p1: f32,
    pub p2: f32,
}

impl Distortion {
    /// True when any coefficient is non-zero (the render post-map runs).
    pub fn any(&self) -> bool {
        self.k1 != 0.0 || self.k2 != 0.0 || self.p1 != 0.0 || self.p2 != 0.0
    }

    /// Forward map: ideal normalized coords → distorted normalized coords.
    fn distort(&self, x: f32, y: f32) -> (f32, f32) {
        let r2 = x * x + y * y;
        let radial = 1.0 + self.k1 * r2 + self.k2 * r2 * r2;
        let dx = 2.0 * self.p1 * x * y + self.p2 * (r2 + 2.0 * x * x);
        let dy = self.p1 * (r2 + 2.0 * y * y) + 2.0 * self.p2 * x * y;
        (x * radial + dx, y * radial + dy)
    }

    /// Inverse map (distorted → ideal), fixed 10 Newton iterations — the
    /// OpenCV `undistort` iteration in f32; fixed count keeps the bytes
    /// deterministic. Only ever run when [`Distortion::any`].
    fn undistort(&self, xd: f32, yd: f32) -> (f32, f32) {
        let (mut x, mut y) = (xd, yd);
        for _ in 0..10 {
            let r2 = x * x + y * y;
            let radial = 1.0 + self.k1 * r2 + self.k2 * r2 * r2;
            let dx = 2.0 * self.p1 * x * y + self.p2 * (r2 + 2.0 * x * x);
            let dy = self.p1 * (r2 + 2.0 * y * y) + 2.0 * self.p2 * x * y;
            x = (xd - dx) / radial;
            y = (yd - dy) / radial;
        }
        (x, y)
    }
}

/// One renderable primitive in the **world frame** — the analytic
/// geometry the bench actually contains (fixtures are axis-aligned
/// boxes, props carry a quaternion, the arm skeleton is capsule
/// segments between joint origins, the goal is a ground disc).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Prim {
    /// Axis-aligned box (fixtures).
    Cuboid {
        center: [f32; 3],
        half: [f32; 3],
        color: [u8; 3],
    },
    /// Oriented box (rotated props / gripper plates), quaternion `[x,y,z,w]`.
    BoxQ {
        center: [f32; 3],
        rot: [f32; 4],
        half: [f32; 3],
        color: [u8; 3],
    },
    /// Capsule segment between two points (arm skeleton).
    Capsule {
        a: [f32; 3],
        b: [f32; 3],
        radius: f32,
        color: [u8; 3],
    },
    /// Cylinder standing on its local axis (prop; `rot` re-orients).
    Cylinder {
        center: [f32; 3],
        rot: [f32; 4],
        half_h: f32,
        radius: f32,
        color: [u8; 3],
    },
    /// Ball (prop).
    Sphere {
        center: [f32; 3],
        r: f32,
        color: [u8; 3],
    },
    /// Horizontal ground disc (the goal marker).
    Disc {
        center: [f32; 3],
        radius: f32,
        color: [u8; 3],
    },
    /// Calibration beacon (U-5): a small ball rendered in its **raw
    /// colour** — unlit, like an LED marker on a real calibration
    /// board. This is what the ≤2 px reprojection gate pins to:
    /// everything else is flat-shaded (colour × shade), a beacon is
    /// exact regardless of the viewing angle.
    Beacon {
        center: [f32; 3],
        r: f32,
        color: [u8; 3],
    },
}

/// The scene as the pinhole renderer consumes it (world frame).
/// Built per tick by the env from fixtures + prop poses + chain points.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SceneView {
    pub prims: Vec<Prim>,
}

/// Quaternion rotate a vector (`q = [x, y, z, w]`).
fn quat_rot(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let qv = [q[0], q[1], q[2]];
    let t = [
        2.0 * (qv[1] * v[2] - qv[2] * v[1]),
        2.0 * (qv[2] * v[0] - qv[0] * v[2]),
        2.0 * (qv[0] * v[1] - qv[1] * v[0]),
    ];
    [
        v[0] + q[3] * t[0] + (qv[1] * t[2] - qv[2] * t[1]),
        v[1] + q[3] * t[1] + (qv[2] * t[0] - qv[0] * t[2]),
        v[2] + q[3] * t[2] + (qv[0] * t[1] - qv[1] * t[0]),
    ]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn norm(v: [f32; 3]) -> f32 {
    dot(v, v).sqrt()
}

fn unit(v: [f32; 3]) -> [f32; 3] {
    let n = norm(v);
    if n <= 0.0 {
        return [0.0; 3];
    }
    [v[0] / n, v[1] / n, v[2] / n]
}

/// A calibrated pinhole camera: world→camera axes + intrinsics +
/// distortion. Pixel centres sit at **integer** coordinates (the OpenCV
/// convention: `u = fx·X/Z + cx` is the pixel index directly), image
/// right = `fwd × up`, image down = its cross with `fwd`.
#[derive(Debug, Clone, PartialEq)]
pub struct PinholeCam {
    pub width: u32,
    pub height: u32,
    pub fx: f32,
    pub fy: f32,
    pub cx: f32,
    pub cy: f32,
    pub dist: Distortion,
    /// Output channels (at least one — the spec validator enforces it).
    pub rgb: bool,
    pub depth: bool,
    /// Camera origin, world frame.
    pub eye: [f32; 3],
    /// Image right direction (unit), world frame.
    right: [f32; 3],
    /// Image down direction (unit), world frame.
    down: [f32; 3],
    /// Optical axis (unit), world frame.
    fwd: [f32; 3],
}

/// Anything closer than this to the camera is clipped (metres) — the
/// wrist camera lives *inside* the arm, so near-clipping is the normal
/// path, not an edge case.
const NEAR: f32 = 0.01;

impl PinholeCam {
    /// Build from an extrinsic pose (`pos`/`look_at`/`up`, world frame)
    /// plus intrinsics. Errors name the offending field (G-7 style):
    /// zero view direction, `up` parallel to it, or non-positive /
    /// non-finite focal lengths.
    #[allow(clippy::too_many_arguments)]
    pub fn from_look(
        pos: [f32; 3],
        look_at: [f32; 3],
        up: [f32; 3],
        width: u32,
        height: u32,
        fx: f32,
        fy: f32,
        cx: f32,
        cy: f32,
        dist: Distortion,
        rgb: bool,
        depth: bool,
    ) -> Result<Self, String> {
        let bad = |v: f32| !v.is_finite() || v <= 0.0;
        if bad(fx) || bad(fy) {
            return Err(format!("camera fx/fy 须为有限正数（像素），当前 fx={fx} fy={fy}"));
        }
        if width == 0 || height == 0 {
            return Err(format!("camera width/height 须 ≥ 1，当前 {width}×{height}"));
        }
        let d = [look_at[0] - pos[0], look_at[1] - pos[1], look_at[2] - pos[2]];
        if norm(d) <= 0.0 {
            return Err("camera look_at == pos：视线方向为零向量".into());
        }
        let fwd = unit(d);
        // image right = up × optical axis (the photographer's right:
        // face along `fwd` with your head along `up` — this puts world
        // +x on the image right for a level camera looking along +z
        // with up +y); image down = right × fwd (the CV y-down raster).
        let r = cross(up, fwd);
        if norm(r) <= 1e-6 {
            return Err("camera up 与视线平行：图像右方向不定（换一个 up，如 [0,0,-1]）".into());
        }
        let right = unit(r);
        let down = cross(right, fwd);
        Ok(Self {
            width,
            height,
            fx,
            fy,
            cx,
            cy,
            dist,
            rgb,
            depth,
            eye: pos,
            right,
            down,
            fwd,
        })
    }

    /// World point → camera-frame coords `(X, Y, Z)` (Z = forward).
    fn to_cam(&self, p: [f32; 3]) -> [f32; 3] {
        let d = [p[0] - self.eye[0], p[1] - self.eye[1], p[2] - self.eye[2]];
        [dot(self.right, d), dot(self.down, d), dot(self.fwd, d)]
    }

    /// Full projection — the calibration probe: world point → distorted
    /// pixel `(u, v)` + camera-frame depth `Z` (metres). `None` when the
    /// point is at/behind the near plane. This is the exact map the
    /// renderer rasterizes through (ideal projection) followed by the
    /// exact distortion post-map it applies, so a visible surface point
    /// projects onto the pixel it was rendered into.
    pub fn project(&self, p: [f32; 3]) -> Option<(f32, f32, f32)> {
        let c = self.to_cam(p);
        if c[2] <= NEAR {
            return None;
        }
        let u = self.fx * c[0] / c[2] + self.cx;
        let v = self.fy * c[1] / c[2] + self.cy;
        if self.dist.any() {
            let (xn, yn) = self.dist.distort((u - self.cx) / self.fx, (v - self.cy) / self.fy);
            return Some((self.fx * xn + self.cx, self.fy * yn + self.cy, c[2]));
        }
        Some((u, v, c[2]))
    }
}

/// Sphere tessellation shared by [`Prim::Sphere`] and [`Prim::Beacon`]:
/// 5 rings (φ = ±60°/±30°/0°) → 4 bands + 2 pole fans, fixed counts.
fn sphere_tris(center: [f32; 3], r: f32, out: &mut Vec<[[f32; 3]; 3]>) {
    const SIDES: usize = 8;
    let quad = |a: [f32; 3], b: [f32; 3], c: [f32; 3], d: [f32; 3], out: &mut Vec<[[f32; 3]; 3]>| {
        out.push([a, b, c]);
        out.push([a, c, d]);
    };
    let pt = |phi_deg: i32, k: usize| -> [f32; 3] {
        let phi = (phi_deg as f32) * (std::f32::consts::PI / 180.0);
        let th = (k as f32) * (std::f32::consts::TAU / SIDES as f32);
        let (c, s) = (phi.cos(), phi.sin());
        let (ct, st) = (th.cos(), th.sin());
        [
            center[0] + r * c * ct,
            center[1] + r * s,
            center[2] + r * c * st,
        ]
    };
    let rings = [-60, -30, 0, 30, 60];
    for w in rings.windows(2) {
        for k in 0..SIDES {
            let k1 = (k + 1) % SIDES;
            quad(pt(w[0], k), pt(w[0], k1), pt(w[1], k1), pt(w[1], k), out);
        }
    }
    let north = [center[0], center[1] + r, center[2]];
    let south = [center[0], center[1] - r, center[2]];
    for k in 0..SIDES {
        let k1 = (k + 1) % SIDES;
        out.push([north, pt(60, k), pt(60, k1)]);
        out.push([south, pt(-60, k1), pt(-60, k)]);
    }
}

/// Triangulate one primitive into world-space triangles (fixed segment
/// counts — coarse low-poly is the honest contract here).
fn prim_tris(p: Prim, out: &mut Vec<[[f32; 3]; 3]>) {
    const SIDES: usize = 8;
    let quad = |a: [f32; 3], b: [f32; 3], c: [f32; 3], d: [f32; 3], out: &mut Vec<[[f32; 3]; 3]>| {
        out.push([a, b, c]);
        out.push([a, c, d]);
    };
    match p {
        Prim::Cuboid { center, half, .. } => {
            let (cx, cy, cz) = (center[0], center[1], center[2]);
            let (hx, hy, hz) = (half[0], half[1], half[2]);
            let v = |i: usize| -> [f32; 3] {
                let sx = if i & 1 != 0 { hx } else { -hx };
                let sy = if i & 2 != 0 { hy } else { -hy };
                let sz = if i & 4 != 0 { hz } else { -hz };
                [cx + sx, cy + sy, cz + sz]
            };
            for (a, b, c, d) in [
                (0b000, 0b010, 0b011, 0b001), // -x
                (0b100, 0b101, 0b111, 0b110), // +x
                (0b000, 0b001, 0b101, 0b100), // -y
                (0b010, 0b110, 0b111, 0b011), // +y
                (0b000, 0b100, 0b110, 0b010), // -z
                (0b001, 0b011, 0b111, 0b101), // +z
            ] {
                quad(v(a), v(b), v(c), v(d), out);
            }
        }
        Prim::BoxQ {
            center,
            rot,
            half,
            ..
        } => {
            let corner = |sx: f32, sy: f32, sz: f32| -> [f32; 3] {
                let l = [sx * half[0], sy * half[1], sz * half[2]];
                let r = quat_rot(rot, l);
                [center[0] + r[0], center[1] + r[1], center[2] + r[2]]
            };
            for sign in [-1.0f32, 1.0] {
                // ±x face
                quad(
                    corner(sign, -1.0, -1.0),
                    corner(sign, -1.0, 1.0),
                    corner(sign, 1.0, 1.0),
                    corner(sign, 1.0, -1.0),
                    out,
                );
                // ±y face
                quad(
                    corner(-1.0, sign, -1.0),
                    corner(-1.0, sign, 1.0),
                    corner(1.0, sign, 1.0),
                    corner(1.0, sign, -1.0),
                    out,
                );
                // ±z face
                quad(
                    corner(-1.0, -1.0, sign),
                    corner(-1.0, 1.0, sign),
                    corner(1.0, 1.0, sign),
                    corner(1.0, -1.0, sign),
                    out,
                );
            }
        }
        Prim::Capsule {
            a,
            b,
            radius,
            ..
        } => {
            let axis = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let n = unit(axis);
            if norm(n) <= 0.0 {
                return;
            }
            let helper = if n[1].abs() < 0.9 { [0.0, 1.0, 0.0] } else { [1.0, 0.0, 0.0] };
            let u1 = unit(cross(n, helper));
            let u2 = cross(n, u1);
            let ring = |base: [f32; 3], k: usize| -> [f32; 3] {
                let th = (k as f32) * (std::f32::consts::TAU / SIDES as f32);
                let (c, s) = (th.cos(), th.sin());
                let off = [
                    radius * (u1[0] * c + u2[0] * s),
                    radius * (u1[1] * c + u2[1] * s),
                    radius * (u1[2] * c + u2[2] * s),
                ];
                [
                    base[0] + off[0],
                    base[1] + off[1],
                    base[2] + off[2],
                ]
            };
            for k in 0..SIDES {
                let k1 = (k + 1) % SIDES;
                let (a0, a1) = (ring(a, k), ring(a, k1));
                let (b0, b1) = (ring(b, k), ring(b, k1));
                quad(a0, a1, b1, b0, out);
                let apex_a = [a[0] - n[0] * radius, a[1] - n[1] * radius, a[2] - n[2] * radius];
                let apex_b = [b[0] + n[0] * radius, b[1] + n[1] * radius, b[2] + n[2] * radius];
                out.push([apex_a, a0, a1]);
                out.push([apex_b, b1, b0]);
            }
        }
        Prim::Cylinder {
            center,
            rot,
            half_h,
            radius,
            ..
        } => {
            let axis = quat_rot(rot, [0.0, 1.0, 0.0]);
            let n = unit(axis);
            let helper = if n[1].abs() < 0.9 { [0.0, 1.0, 0.0] } else { [1.0, 0.0, 0.0] };
            let u1 = unit(cross(n, helper));
            let u2 = cross(n, u1);
            let ring = |s: f32, k: usize| -> [f32; 3] {
                let th = (k as f32) * (std::f32::consts::TAU / SIDES as f32);
                let (c, sn) = (th.cos(), th.sin());
                let off = [
                    radius * (u1[0] * c + u2[0] * sn),
                    radius * (u1[1] * c + u2[1] * sn),
                    radius * (u1[2] * c + u2[2] * sn),
                ];
                [
                    center[0] + s * half_h * n[0] + off[0],
                    center[1] + s * half_h * n[1] + off[1],
                    center[2] + s * half_h * n[2] + off[2],
                ]
            };
            let top = [
                center[0] + half_h * n[0],
                center[1] + half_h * n[1],
                center[2] + half_h * n[2],
            ];
            let bot = [
                center[0] - half_h * n[0],
                center[1] - half_h * n[1],
                center[2] - half_h * n[2],
            ];
            for k in 0..SIDES {
                let k1 = (k + 1) % SIDES;
                quad(ring(-1.0, k), ring(-1.0, k1), ring(1.0, k1), ring(1.0, k), out);
                out.push([bot, ring(-1.0, k), ring(-1.0, k1)]);
                out.push([top, ring(1.0, k1), ring(1.0, k)]);
            }
        }
        Prim::Sphere { center, r, .. } => {
            sphere_tris(center, r, out);
        }
        Prim::Beacon { center, r, .. } => {
            sphere_tris(center, r, out);
        }
        Prim::Disc { center, radius, .. } => {
            let fan = |y: f32, out: &mut Vec<[[f32; 3]; 3]>| {
                let rim = |k: usize| -> [f32; 3] {
                    let th = (k as f32) * (std::f32::consts::TAU / SIDES as f32);
                    let (c, s) = (th.cos(), th.sin());
                    [
                        center[0] + radius * c,
                        y,
                        center[2] + radius * s,
                    ]
                };
                let c = [center[0], y, center[2]];
                for k in 0..SIDES {
                    let k1 = (k + 1) % SIDES;
                    out.push([c, rim(k), rim(k1)]);
                }
            };
            fan(center[1], out); // double-sided via shading (no culling)
        }
    }
}

/// Clip a camera-space triangle against `Z ≥ NEAR` (Sutherland–Hodgman
/// on the single near plane): 0 tris behind the camera, 1 clipped tri,
/// or a quad → 2 tris.
fn clip_near(tri: &[[f32; 3]; 3]) -> Vec<[f32; 3]> {
    let mut poly: Vec<[f32; 3]> = tri.to_vec();
    let mut out = Vec::with_capacity(5);
    let n = poly.len();
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        let (ain, bin) = (a[2] >= NEAR, b[2] >= NEAR);
        if ain {
            out.push(a);
        }
        if ain != bin {
            let t = (NEAR - a[2]) / (b[2] - a[2]);
            out.push([
                a[0] + t * (b[0] - a[0]),
                a[1] + t * (b[1] - a[1]),
                NEAR,
            ]);
        }
    }
    poly.clear();
    out
}

/// One pinhole frame: ideal-pinhole rasterization + distortion post-map.
/// `rgb` is row-major RGB8 (background [24,27,36] — the ortho ground
/// colour); `depth` is row-major **millimetre** u16 (0 = no return).
#[derive(Debug, Clone, PartialEq)]
pub struct PinholeFrame {
    pub rgb: Option<Vec<u8>>,
    pub depth: Option<Vec<u16>>,
}

/// Marker-pass depth tolerance (metres): a [`Prim::Beacon`] paints over
/// the surface in front of it when that surface is within this distance
/// of the beacon along the ray — the standard robot-viz TCP-marker
/// behaviour (the tip beacon lives INSIDE the wrist capsule, which is
/// exactly why the marker pass exists). Real occluders (a table edge
/// tens of cm closer) still hide the beacon, and the depth channel
/// keeps the true surface — beacons never write it.
const MARKER_TOL: f32 = 0.10;

/// Render the scene from one pinhole camera. Deterministic: fixed
/// tessellation, f32 edge-function rasterization at pixel centres
/// (integer coords), z-buffered with strict `>` (first write wins
/// ties), head-light flat shading, distortion as a nearest-neighbour
/// inverse map that runs only when a coefficient is non-zero.
pub fn render_pinhole(cam: &PinholeCam, scene: &SceneView) -> PinholeFrame {
    let (w, h) = (cam.width, cam.height);
    let n = (w as usize) * (h as usize);
    let mut rgb = cam.rgb.then(|| vec![[24u8, 27, 36]; n].concat());
    let mut depth = cam.depth.then(|| vec![0u16; n]);
    let mut zbuf = vec![0.0f32; n]; // stores 1/Z; 0 = nothing drawn yet
    let mut tris: Vec<[[f32; 3]; 3]> = Vec::with_capacity(256);
    // two passes: the lit scene, then the marker pass (beacons only)
    for marker_pass in [false, true] {
        for prim in &scene.prims {
            let is_beacon = matches!(prim, Prim::Beacon { .. });
            if is_beacon != marker_pass {
                continue;
            }
            tris.clear();
            prim_tris(*prim, &mut tris);
            let base = match prim {
                Prim::Cuboid { color, .. }
                | Prim::BoxQ { color, .. }
                | Prim::Capsule { color, .. }
                | Prim::Cylinder { color, .. }
                | Prim::Sphere { color, .. }
                | Prim::Disc { color, .. }
                | Prim::Beacon { color, .. } => *color,
            };
            let unlit = is_beacon;
            for tw in &tris {
                // near clip in camera space
                let cam_tri: Vec<[f32; 3]> = tw
                    .iter()
                    .map(|p| cam.to_cam(*p))
                    .collect::<Vec<_>>();
                let poly: Vec<[f32; 3]> = clip_near(&[cam_tri[0], cam_tri[1], cam_tri[2]]);
                if poly.len() < 3 {
                    continue;
                }
                // flat shade: head-light along the optical axis, two-sided
                // (beacons are emissive — the calibration targets stay raw)
                let e1 = [tw[1][0] - tw[0][0], tw[1][1] - tw[0][1], tw[1][2] - tw[0][2]];
                let e2 = [tw[2][0] - tw[0][0], tw[2][1] - tw[0][1], tw[2][2] - tw[0][2]];
                let nrm = cross(e1, e2);
                let nl = norm(nrm);
                let lam = if nl > 0.0 { dot(unit(nrm), cam.fwd).abs() } else { 0.0 };
                let shade = if unlit { 1.0 } else { 0.45 + 0.55 * lam };
                let col = [
                    ((base[0] as f32) * shade).round().min(255.0) as u8,
                    ((base[1] as f32) * shade).round().min(255.0) as u8,
                    ((base[2] as f32) * shade).round().min(255.0) as u8,
                ];
                // project (ideal pinhole) the clipped polygon as a fan
                let mut pj: Vec<(f32, f32, f32)> = Vec::with_capacity(poly.len());
                for c in &poly {
                    pj.push((cam.fx * c[0] / c[2] + cam.cx, cam.fy * c[1] / c[2] + cam.cy, c[2]));
                }
                let v0 = pj[0];
                for k in 1..pj.len() - 1 {
                    let (v1, v2) = (pj[k], pj[k + 1]);
                    let area = (v1.0 - v0.0) * (v2.1 - v0.1) - (v2.0 - v0.0) * (v1.1 - v0.1);
                    if area.abs() < 1e-9 {
                        continue;
                    }
                    let xmin = v0.0.min(v1.0).min(v2.0);
                    let xmax = v0.0.max(v1.0).max(v2.0);
                    let ymin = v0.1.min(v1.1).min(v2.1);
                    let ymax = v0.1.max(v1.1).max(v2.1);
                    let x0 = ((xmin.floor() as i32).max(0)) as u32;
                    let x1 = ((xmax.ceil() as i32).min(w as i32 - 1)).max(0) as u32;
                    let y0 = ((ymin.floor() as i32).max(0)) as u32;
                    let y1 = ((ymax.ceil() as i32).min(h as i32 - 1)).max(0) as u32;
                    for yy in y0..=y1 {
                        for xx in x0..=x1 {
                            let (px, py) = (xx as f32, yy as f32);
                            let w0 = (v1.0 - v0.0) * (py - v0.1) - (v1.1 - v0.1) * (px - v0.0);
                            let w1 = (v2.0 - v1.0) * (py - v1.1) - (v2.1 - v1.1) * (px - v1.0);
                            let w2 = (v0.0 - v2.0) * (py - v2.1) - (v0.1 - v2.1) * (px - v2.0);
                            // inside test: all three edge functions carry
                            // the winding's sign, so dividing by the
                            // SIGNED area normalizes both orientations
                            // (a `-w` flip here would silently reject
                            // every CW-wound triangle — the disc fan bug).
                            // PAIRING: w0 = cross(v1−v0, p−v0) is the
                            // sub-area OPPOSITE v2 — the weight of v2,
                            // not v0. (b0,b1,b2) = (w1,w2,w0)/area; a
                            // direct (w0,w1,w2) pairing leaves coverage
                            // correct (the ≥0 test is symmetric) but
                            // silently corrupts the 1/z interpolation on
                            // every depth-varying triangle.
                            let (b0, b1, b2) = (w1 / area, w2 / area, w0 / area);
                            if b0 < 0.0 || b1 < 0.0 || b2 < 0.0 {
                                continue;
                            }
                            // screen-linear 1/z interpolation (exact)
                            let iz = b0 / v0.2 + b1 / v1.2 + b2 / v2.2;
                            let idx = (yy as usize) * w as usize + xx as usize;
                            if iz > zbuf[idx] {
                                zbuf[idx] = iz;
                                let mm = (1.0 / iz * 1000.0).round();
                                let mm = mm.clamp(1.0, u16::MAX as f32) as u16;
                                if let Some(buf) = rgb.as_mut() {
                                    let i = idx * 3;
                                    buf[i..i + 3].copy_from_slice(&col);
                                }
                                if let Some(buf) = depth.as_mut() {
                                    buf[idx] = mm;
                                }
                            } else if marker_pass && zbuf[idx] > 0.0 {
                                // marker pass: paint (RGB only) over a
                                // surface that is within MARKER_TOL of
                                // the beacon along the ray — its own
                                // arm, not a real occluder
                                let z_beacon = 1.0 / iz;
                                let z_surface = 1.0 / zbuf[idx];
                                if z_surface > 0.0 && z_beacon - z_surface <= MARKER_TOL {
                                    if let Some(buf) = rgb.as_mut() {
                                        let i = idx * 3;
                                        buf[i..i + 3].copy_from_slice(&col);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    // Distortion post-map: for each output pixel (distorted coords),
    // invert to the ideal pixel and nearest-sample. Skipped entirely at
    // zero coefficients (byte-identical to "no distortion").
    if cam.dist.any() {
        // one inverse map per output pixel, shared by both buffers
        let mut ideal_xy: Vec<(i64, i64)> = Vec::with_capacity(n);
        for yy in 0..h {
            for xx in 0..w {
                let (xn, yn) = cam.dist.undistort(
                    (xx as f32 - cam.cx) / cam.fx,
                    (yy as f32 - cam.cy) / cam.fy,
                );
                let ui = (cam.fx * xn + cam.cx).round() as i64;
                let vi = (cam.fy * yn + cam.cy).round() as i64;
                let inside = ui >= 0 && vi >= 0 && ui < w as i64 && vi < h as i64;
                ideal_xy.push((if inside { ui } else { -1 }, if inside { vi } else { -1 }));
            }
        }
        if let Some(buf) = &rgb {
            let mut out = vec![0u8; n * 3];
            for i in 0..n {
                out[i * 3..i * 3 + 3].copy_from_slice(&[24, 27, 36]);
            }
            for (i, &(ui, vi)) in ideal_xy.iter().enumerate() {
                if ui < 0 {
                    continue; // stays background
                }
                let s = (vi as usize * w as usize + ui as usize) * 3;
                out[i * 3..i * 3 + 3].copy_from_slice(&buf[s..s + 3]);
            }
            rgb = Some(out);
        }
        if let Some(buf) = &depth {
            let mut out = vec![0u16; n];
            for (i, &(ui, vi)) in ideal_xy.iter().enumerate() {
                if ui < 0 {
                    continue;
                }
                out[i] = buf[vi as usize * w as usize + ui as usize];
            }
            depth = Some(out);
        }
    }
    PinholeFrame { rgb, depth }
}

/// Encode a millimetre depth buffer as a 16-bit grayscale PNG (big-endian
/// samples per the PNG spec) — the `depth.{name}` track's frame format.
pub fn encode_depth_png(depth_mm: &[u16], w: u32, h: u32) -> Result<Vec<u8>, String> {
    let mut be = Vec::with_capacity(depth_mm.len() * 2);
    for v in depth_mm {
        be.extend_from_slice(&v.to_be_bytes());
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Sixteen);
        let mut writer = enc.write_header().map_err(|e| format!("png header: {e}"))?;
        writer
            .write_image_data(&be)
            .map_err(|e| format!("png data: {e}"))?;
    }
    Ok(out)
}

/// Encoded camera frame pair: RGB8 PNG bytes + 16-bit depth PNG bytes
/// (either side `None` when the camera doesn't export that channel).
pub type PngPair = (Option<Vec<u8>>, Option<Vec<u8>>);

/// Render + encode in one call: raw pinhole frame → RGB8 PNG bytes +
/// 16-bit depth PNG bytes.
pub fn capture_pinhole_png(cam: &PinholeCam, scene: &SceneView) -> Result<PngPair, String> {
    let frame = render_pinhole(cam, scene);
    let rgb_png = match &frame.rgb {
        Some(buf) => Some(encode_png(buf, cam.width, cam.height)?),
        None => None,
    };
    let depth_png = match &frame.depth {
        Some(buf) => Some(encode_depth_png(buf, cam.width, cam.height)?),
        None => None,
    };
    Ok((rgb_png, depth_png))
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

#[cfg(test)]
mod pinhole_tests {
    use super::*;

    fn overhead(w: u32, h: u32, dist: Distortion, rgb: bool, depth: bool) -> PinholeCam {
        // 0.6 m above the bench centre, looking at the table top — a
        // natural overhead photo (+x right, +z image-up).
        PinholeCam::from_look(
            [0.32, 1.10, 0.0],
            [0.32, 0.50, 0.0],
            [0.0, 0.0, 1.0],
            w,
            h,
            240.0,
            240.0,
            w as f32 / 2.0,
            h as f32 / 2.0,
            dist,
            rgb,
            depth,
        )
        .expect("overhead cam builds")
    }

    fn demo_scene() -> SceneView {
        SceneView {
            prims: vec![
                Prim::Cuboid {
                    center: [0.32, 0.25, 0.0],
                    half: [0.28, 0.25, 0.32],
                    color: [56, 68, 88],
                },
                Prim::BoxQ {
                    center: [0.20, 0.525, 0.05],
                    rot: [0.0, 0.0, 0.0, 1.0],
                    half: [0.025; 3],
                    color: [255, 206, 110],
                },
                Prim::Disc {
                    center: [0.45, 0.5015, 0.02],
                    radius: 0.045,
                    color: [64, 200, 132],
                },
            ],
        }
    }

    #[test]
    fn pinhole_projects_the_optical_axis_to_the_principal_point() {
        let cam = PinholeCam::from_look(
            [0.0, 0.0, -2.0],
            [0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            160,
            120,
            100.0,
            100.0,
            80.0,
            60.0,
            Distortion::default(),
            true,
            false,
        )
        .expect("cam builds");
        let (u, v, z) = cam.project([0.0, 0.0, 0.0]).expect("in front");
        assert!((u - 80.0).abs() < 1e-4 && (v - 60.0).abs() < 1e-4, "{u} {v}");
        assert!((z - 2.0).abs() < 1e-5, "Z is the camera-frame depth");
        // a point above the axis projects above the principal point
        let (_, v_up, _) = cam.project([0.0, 0.4, 0.0]).expect("in front");
        assert!(v_up < v, "image up must shrink v");
        // behind the near plane → None
        assert!(cam.project([0.0, 0.0, -3.0]).is_none());
    }

    #[test]
    fn from_look_names_the_offending_field() {
        let err = PinholeCam::from_look(
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            160,
            120,
            100.0,
            100.0,
            80.0,
            60.0,
            Distortion::default(),
            true,
            false,
        )
        .unwrap_err();
        assert!(err.contains("look_at"), "{err}");
        let err = PinholeCam::from_look(
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 1.0],
            160,
            120,
            100.0,
            100.0,
            80.0,
            60.0,
            Distortion::default(),
            true,
            false,
        )
        .unwrap_err();
        assert!(err.contains("up"), "{err}");
        let err = PinholeCam::from_look(
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0],
            160,
            120,
            -1.0,
            100.0,
            80.0,
            60.0,
            Distortion::default(),
            true,
            false,
        )
        .unwrap_err();
        assert!(err.contains("fx"), "{err}");
    }

    /// The calibration-gate core: a projected world point lands on the
    /// pixel its surface rasterized into (colour + depth agree).
    #[test]
    fn projected_points_land_on_their_rendered_pixels() {
        let cam = overhead(160, 120, Distortion::default(), true, true);
        let frame = render_pinhole(&cam, &demo_scene());
        // cube centre → the top face carries the cube colour there
        let cube_c = [0.20, 0.525, 0.05];
        let (u, v, z) = cam.project(cube_c).expect("cube in front");
        let (px, py) = (u.round() as i32, v.round() as i32);
        let rgb = frame.rgb.as_ref().expect("rgb on");
        let at = |x: i32, y: i32| {
            let i = (y as usize * cam.width as usize + x as usize) * 3;
            [rgb[i], rgb[i + 1], rgb[i + 2]]
        };
        let mut best = (i32::MAX, [0u8; 3]);
        for dy in -1..=1 {
            for dx in -1..=1 {
                let c = at(px + dx, py + dy);
                if c == [255, 206, 110] {
                    best = (dx.abs().pow(2) + dy.abs().pow(2), c);
                }
            }
        }
        assert_eq!(best.1, [255, 206, 110], "cube colour within 1 px of the projection");
        // depth: the point sits 5 cm inside the cube, the surface 2.5 cm
        // closer along the ray — the z-buffer holds the SURFACE, not the
        // interior point (camera is 0.575 m above, face at 0.55).
        let depth = frame.depth.as_ref().expect("depth on");
        let mm = depth[py as usize * cam.width as usize + px as usize] as f32;
        assert!(
            (mm - (z - 0.025) * 1000.0).abs() <= 12.0,
            "depth {mm} vs face {} mm",
            (z - 0.025) * 1000.0
        );
    }

    /// Distortion: pixels move, and project() (forward map) still agrees
    /// with the rendered image (inverse-mapped post-process) — the same
    /// ≤2 px gate, now through the lens model.
    #[test]
    fn distortion_moves_pixels_and_project_still_agrees() {
        let dist = Distortion {
            k1: 0.18,
            k2: 0.02,
            p1: 0.0,
            p2: 0.001,
        };
        let plain = overhead(160, 120, Distortion::default(), true, true);
        let lensed = overhead(160, 120, dist, true, true);
        let f_plain = render_pinhole(&plain, &demo_scene());
        let f_lens = render_pinhole(&lensed, &demo_scene());
        assert_ne!(f_plain, f_lens, "the lens must move pixels");

        let cube_c = [0.20, 0.525, 0.05];
        let (u, v, _) = lensed.project(cube_c).expect("in front");
        let (px, py) = (u.round() as i32, v.round() as i32);
        let rgb = f_lens.rgb.as_ref().expect("rgb on");
        let at = |x: i32, y: i32| {
            let i = (y as usize * lensed.width as usize + x as usize) * 3;
            [rgb[i], rgb[i + 1], rgb[i + 2]]
        };
        // nearest cube-coloured pixel: strictly < 2 px (Chebyshev)
        let mut best: Option<(i32, [u8; 3])> = None;
        for dy in -3i32..=3 {
            for dx in -3i32..=3 {
                if at(px + dx, py + dy) == [255, 206, 110] {
                    let d = dx.abs().max(dy.abs());
                    if best.is_none_or(|(bd, _)| d < bd) {
                        best = Some((d, [255, 206, 110]));
                    }
                }
            }
        }
        let (d, _) = best.expect("cube visible through the lens");
        assert!(d < 2, "reprojection error {d} px must be < 2");
    }

    #[test]
    fn depth_channel_is_millimetre_u16_png() {
        let cam = overhead(64, 48, Distortion::default(), false, true);
        let frame = render_pinhole(&cam, &demo_scene());
        assert!(frame.rgb.is_none(), "rgb off ⇒ no rgb buffer");
        let depth = frame.depth.as_ref().expect("depth on");
        // table top is 0.6 m below the camera
        let centre = depth[(24 * 64 + 32) as usize];
        assert!((centre as i32 - 600).abs() <= 2, "table depth {centre} mm");
        let png = encode_depth_png(depth, 64, 48).expect("depth png");
        assert_eq!(&png[0..8], b"\x89PNG\r\n\x1a\n");
        // 16-bit grayscale IHDR: bit-depth byte (24) = 16, colour type (25) = 0
        assert_eq!(&png[24..26], &[16, 0], "bit depth 16 / grayscale in IHDR");
    }

    /// The wrist camera lives INSIDE the arm — geometry behind the near
    /// plane must clip instead of corrupting the frame, and the depth
    /// channel stays the sensor-style millimetre buffer.
    #[test]
    fn near_plane_clips_behind_camera_geometry() {
        let cam = PinholeCam::from_look(
            [0.30, 0.55, 0.02],
            [0.42, 0.40, 0.02],
            [0.0, 0.0, -1.0],
            80,
            60,
            90.0,
            90.0,
            40.0,
            30.0,
            Distortion::default(),
            true,
            true,
        )
        .expect("cam builds");
        let scene = SceneView {
            prims: vec![
                Prim::Capsule {
                    a: [0.10, 0.60, 0.0],
                    b: [0.30, 0.55, 0.0],
                    radius: 0.012,
                    color: [91, 141, 239],
                },
                Prim::BoxQ {
                    center: [0.44, 0.50, 0.02],
                    rot: [0.0, 0.0, 0.0, 1.0],
                    half: [0.025; 3],
                    color: [255, 206, 110],
                },
            ],
        };
        let f1 = render_pinhole(&cam, &scene);
        let f2 = render_pinhole(&cam, &scene);
        assert_eq!(f1, f2, "deterministic");
        // the cube in front is visible and depth-sane
        let (u, v, z) = cam.project([0.44, 0.50, 0.02]).expect("cube in front");
        let depth = f1.depth.as_ref().expect("depth on");
        let mm = depth[v.round() as usize * 80 + u.round() as usize] as f32;
        assert!((mm - z * 1000.0).abs() <= 30.0, "depth {mm} mm vs {z} m");
    }
}

#[cfg(test)]
mod disc_probe {
    use super::*;

    #[test]
    fn disc_renders_over_the_table() {
        let cam = PinholeCam::from_look(
            [0.32, 1.10, 0.0],
            [0.32, 0.50, 0.0],
            [0.0, 0.0, 1.0],
            320,
            240,
            240.0,
            240.0,
            160.0,
            120.0,
            Distortion::default(),
            true,
            false,
        )
        .unwrap();
        let scene = SceneView {
            prims: vec![
                Prim::Cuboid {
                    center: [0.32, 0.25, 0.0],
                    half: [0.28, 0.25, 0.32],
                    color: [56, 68, 88],
                },
                Prim::Disc {
                    center: [0.2891319, 0.502, -4.06e-5],
                    radius: 0.045,
                    color: [64, 200, 132],
                },
            ],
        };
        let f = render_pinhole(&cam, &scene);
        let rgb = f.rgb.expect("rgb");
        let (u, v, _) = cam.project([0.2891319, 0.502, -4.06e-5]).unwrap();
        println!("proj = {u} {v}");
        let w = cam.width as usize;
        let mut count = 0;
        for px in rgb.as_chunks::<3>().0 {
            if px == &[64, 200, 132] {
                count += 1;
            }
        }
        println!("goal px = {count}");
        let at = |x: usize, y: usize| {
            let i = (y * w + x) * 3;
            [rgb[i], rgb[i + 1], rgb[i + 2]]
        };
        println!("centre px = {:?}", at(u.round() as usize, v.round() as usize));
        assert!(count > 0, "the disc must render");
    }
}

#[cfg(test)]
mod clipped_sliver_regression {
    use super::*;

    #[test]
    fn clipped_sliver_interpolates_the_true_depth() {
        let cam = PinholeCam::from_look(
            [0.418, 0.675, 0.0],
            [0.568, 0.535, 0.0],
            [0.0, 0.0, 1.0],
            160,
            120,
            90.0,
            90.0,
            80.0,
            60.0,
            Distortion::default(),
            true,
            true,
        )
        .unwrap();
        // ground top face tri(2,6,7): verts (−2,0,−1.5) (2,0,−1.5) (2,0,1.5)
        let tri = [
            [-2.0f32, 0.0, -1.5],
            [2.0, 0.0, -1.5],
            [2.0, 0.0, 1.5],
        ];
        let cam_tri: Vec<[f32; 3]> = tri.iter().map(|p| cam.to_cam(*p)).collect();
        println!("cam verts: {cam_tri:?}");
        let poly = clip_near(&[cam_tri[0], cam_tri[1], cam_tri[2]]);
        println!("clipped: {poly:?}");
        let pj: Vec<(f32, f32, f32)> = poly
            .iter()
            .map(|c| (cam.fx * c[0] / c[2] + cam.cx, cam.fy * c[1] / c[2] + cam.cy, c[2]))
            .collect();
        println!("projected: {pj:?}");
        // The centre pixel ray hits the ground at ~1110 mm. Regression:
        // pairing the edge functions as (w0,w1,w2) — instead of the
        // correct opposite-vertex (w1,w2,w0) — interpolated ~12 mm here
        // (coverage stayed right, depth silently rotted).
        let v0 = pj[0];
        let mut best_z = 0.0f32;
        for k in 1..pj.len() - 1 {
            let (v1, v2) = (pj[k], pj[k + 1]);
            let area = (v1.0 - v0.0) * (v2.1 - v0.1) - (v2.0 - v0.0) * (v1.1 - v0.1);
            let (px, py) = (80.0f32, 60.0f32);
            let w0 = (v1.0 - v0.0) * (py - v0.1) - (v1.1 - v0.1) * (px - v0.0);
            let w1 = (v2.0 - v1.0) * (py - v1.1) - (v2.1 - v1.1) * (px - v1.0);
            let w2 = (v0.0 - v2.0) * (py - v2.1) - (v0.1 - v2.1) * (px - v2.0);
            let (b0, b1, b2) = (w1 / area, w2 / area, w0 / area);
            if b0 < 0.0 || b1 < 0.0 || b2 < 0.0 {
                continue;
            }
            let iz = b0 / v0.2 + b1 / v1.2 + b2 / v2.2;
            best_z = best_z.max(1.0 / iz);
        }
        // true ground hit along the centre ray: 0.675 m / fwd.y (0.682)
        assert!(
            (best_z - 0.9897).abs() < 0.02,
            "the clipped ground sliver must interpolate the true plane depth, got {best_z} m"
        );
    }
}
