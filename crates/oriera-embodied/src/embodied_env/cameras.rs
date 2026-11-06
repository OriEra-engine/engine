//! Camera output types and render helpers (M3-A): the per-tick
//! `CameraFrame` / `CameraShot` records and the EE-mount quaternion /
//! top-down stamping helpers the env's capture path calls. The
//! rendering itself additionally needs the `cam` feature; the frame
//! type compiles without it.

#[cfg(feature = "cam")]
use oriera_subsys::camera::{CameraView, PinholeCam};
#[cfg(feature = "cam")]
use oriera_subsys::rigid3d::PropShape;

/// One captured camera frame (per tick, per camera): PNG bytes of the
/// channels the camera exports. The legacy top-down `--cam` view emits
/// `cam = "top"` with RGB only; spec cameras emit RGB and/or a
/// millimetre u16 depth PNG. Drained by `TableTopEnv::take_frames`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraFrame {
    /// LeRobot feature suffix (`observation.images.{cam}` / `depth.{cam}`).
    pub cam: String,
    /// RGB8 PNG bytes, when the camera exports RGB.
    pub rgb_png: Option<Vec<u8>>,
    /// 16-bit grayscale PNG (millimetres, 0 = no return), when the
    /// camera exports depth.
    pub depth_png: Option<Vec<u8>>,
}

/// One raw (unencoded) spec-camera render — the calibration probe's
/// input (feature `cam`): pixel buffers plus the camera that produced
/// them, so a caller can `project()` world points onto the same image.
#[cfg(feature = "cam")]
#[derive(Debug, Clone, PartialEq)]
pub struct CameraShot {
    pub cam: String,
    pub w: u32,
    pub h: u32,
    /// Row-major RGB8.
    pub rgb: Option<Vec<u8>>,
    /// Row-major millimetre depth.
    pub depth: Option<Vec<u16>>,
    /// The calibrated camera (resolved extrinsics) this frame came from.
    pub pinhole: PinholeCam,
}

/// Quaternion `[x, y, z, w]` rotate a vector — the EE-frame mount math
/// (mirrors `oriera_subsys::camera`'s private helper; kept here so the
/// env never reaches into the renderer's internals).
#[cfg(feature = "cam")]
pub(crate) fn quat_rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
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

/// Skeleton capsule radius of one link: the shape's thin dimension
/// (capsule radius / box min-half / ball radius), floored so hairline
/// links stay visible in the perspective render.
#[cfg(feature = "cam")]
pub(crate) fn link_radius(shape: &oriera_subsys::LinkShape) -> f32 {
    let r = match shape {
        oriera_subsys::LinkShape::Capsule { radius, .. } => *radius,
        oriera_subsys::LinkShape::Box { half } => half.iter().cloned().fold(f32::MAX, f32::min),
        oriera_subsys::LinkShape::Ball { radius } => *radius,
        _ => 0.012,
    };
    r.max(0.008)
}

/// Stamp a non-default prop footprint onto a rendered RGB frame
/// (top-down): filled circle for cylinder/sphere, rect for a custom
/// cuboid. Same prop colour as the legacy cube marker.
#[cfg(feature = "cam")]
pub(crate) fn stamp_prop(view: &CameraView, rgb: &mut [u8], pos: [f32; 3], shape: PropShape) {
    let prop_colour = [255u8, 206, 110];
    let scale = |metres: f32, extent: f32, px: u32| (metres / extent * px as f32).round() as i32;
    // Same projection as `CameraView::px` (private in subsys; kept in
    // lockstep — deterministic integer rounding).
    let px = ((pos[0] - (view.center[0] - view.extent[0] / 2.0)) / view.extent[0] * view.w as f32)
        .round() as i32;
    let py = ((pos[2] - (view.center[1] - view.extent[1] / 2.0)) / view.extent[1] * view.h as f32)
        .round() as i32;
    let put = |rgb: &mut [u8], x: i32, y: i32| {
        if x < 0 || y < 0 || x >= view.w as i32 || y >= view.h as i32 {
            return;
        }
        let i = (y as u32 * view.w + x as u32) as usize * 3;
        rgb[i..i + 3].copy_from_slice(&prop_colour);
    };
    match shape {
        // A custom cuboid would double-draw identically through the
        // legacy marker; only the default shape takes the fast path, so
        // stamp rounded extents here (covers e.g. half=[0.03,0.01,0.04]).
        PropShape::Cuboid { half } => {
            let (hw, hh) = (
                scale(half[0], view.extent[0], view.w),
                scale(half[2], view.extent[1], view.h),
            );
            for y in (py - hh)..=(py + hh) {
                for x in (px - hw)..=(px + hw) {
                    put(rgb, x, y);
                }
            }
        }
        PropShape::Cylinder { .. } | PropShape::Sphere { .. } => {
            let (fx, _) = shape.footprint();
            let r = scale(fx, view.extent[0], view.w);
            for y in (py - r)..=(py + r) {
                for x in (px - r)..=(px + r) {
                    if (x - px).pow(2) + (y - py).pow(2) <= r * r {
                        put(rgb, x, y);
                    }
                }
            }
        }
    }
}

/// Top-down colour of the furniture beyond the legacy table slab
/// (`fixtures[2..]`: bin walls, cabinet panels) — a lighter slate than
/// the table, so a wall standing on the table reads as its own footprint.
#[cfg(feature = "cam")]
pub(crate) const TOP_FURNITURE_COLOUR: [u8; 3] = [112, 126, 154];

/// Paint the furniture the legacy two-slab view never knew about
/// (`fixtures[2..]`, as `(center, half)`) into a rendered top-down frame
/// **under** what is already drawn (P2-R). A per-pixel height stands in
/// for the depth the orthographic view lacks: a table pixel holds
/// `table_top`, a background pixel −∞, and a pixel the goal disc / arm
/// skeleton / a prop already owns is never repainted (painter's order
/// furniture → goal → arm → props, without touching the byte-frozen
/// `render_top_down`). A fixture paints a pixel only when its top face
/// is strictly above the height already there — the pinhole's strict
/// `>` z-test — so a floor flush with the table stays table-coloured
/// exactly as in the pinhole render, while walls standing on it show.
/// Same projection and integer rounding as [`stamp_prop`]; deterministic.
#[cfg(feature = "cam")]
pub(crate) fn stamp_fixtures_under(
    view: &CameraView,
    rgb: &mut [u8],
    table_top: f32,
    fixtures: &[([f32; 3], [f32; 3])],
) {
    const TABLE: [u8; 3] = [56, 68, 88];
    const BACKGROUND: [u8; 3] = [24, 27, 36];
    if fixtures.is_empty() {
        return; // nothing beyond the legacy slabs: the frame stays as drawn
    }
    let n = (view.w * view.h) as usize;
    let mut height: Vec<f32> = (0..n)
        .map(|i| match [rgb[i * 3], rgb[i * 3 + 1], rgb[i * 3 + 2]] {
            TABLE => table_top,
            BACKGROUND => f32::NEG_INFINITY,
            _ => f32::INFINITY,
        })
        .collect();
    let scale = |metres: f32, extent: f32, px: u32| (metres / extent * px as f32).round() as i32;
    for &(center, half) in fixtures {
        let top = center[1] + half[1];
        // `CameraView::px` (private in subsys) — kept in lockstep, as in
        // `stamp_prop`.
        let px = ((center[0] - (view.center[0] - view.extent[0] / 2.0)) / view.extent[0]
            * view.w as f32)
            .round() as i32;
        let py = ((center[2] - (view.center[1] - view.extent[1] / 2.0)) / view.extent[1]
            * view.h as f32)
            .round() as i32;
        let (hw, hh) = (
            scale(half[0], view.extent[0], view.w),
            scale(half[2], view.extent[1], view.h),
        );
        for y in (py - hh)..=(py + hh) {
            for x in (px - hw)..=(px + hw) {
                if x < 0 || y < 0 || x >= view.w as i32 || y >= view.h as i32 {
                    continue;
                }
                let i = (y as u32 * view.w + x as u32) as usize;
                if top > height[i] {
                    height[i] = top;
                    rgb[i * 3..i * 3 + 3].copy_from_slice(&TOP_FURNITURE_COLOUR);
                }
            }
        }
    }
}
