//! M3-A integration tests (PLAN-embodied-general §四 M3 / §9.1 U-5):
//! spec-declared calibrated cameras through the pinhole pipeline —
//! multi RGB/depth tracks out of one env, EE-mounted wrist extrinsics,
//! and the ≤2 px reprojection gate between `project()` and the
//! rendered pixels. The legacy top-down path stays byte-frozen (its
//! determinism tests live in `embodied_env.rs`; the cross-commit byte
//! anchor is the acceptance diff against the pre-M3 worktree).
#![cfg(all(feature = "rigid3d", feature = "cam", feature = "spec"))]

use oriera_embodied::embodied_env::{CamAttach, EmbodiedEnv, TableTopEnv};
use oriera_embodied::lerobot::{CameraTrack, LeRobotV21};
use oriera_embodied::spec::SceneSpec;
use oriera_subsys::camera::PinholeCam;

/// The shipped lab demo: golden spec + wrist (EE, 160×120 RGB+depth,
/// k1=0.15) + top (world, 320×240 RGB).
fn demo_spec() -> SceneSpec {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/specs/lab/wrist-cam-demo.json");
    SceneSpec::load_file(std::path::Path::new(path))
        .expect("wrist-cam-demo resolves against the official library")
        .spec
}

fn cam_env() -> TableTopEnv {
    let spec = demo_spec();
    let mut env: TableTopEnv = spec.to_env();
    env.set_cameras(spec.scene.cameras.clone());
    env.randomize = false;
    env
}

/// A tiny deterministic joint wiggle (pure function of the step).
fn wiggle(step: u64) -> Vec<f32> {
    let s = step as f32 * 0.02;
    vec![s.sin() * 0.3, 0.2, -0.4, 0.5, 0.0]
}

const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

#[test]
fn spec_cameras_capture_multi_rgb_depth_tracks() {
    let mut e1 = cam_env();
    let mut e2 = cam_env();
    e1.reset(11);
    e2.reset(11);
    let mut per_cam: std::collections::BTreeMap<String, (usize, usize)> =
        std::collections::BTreeMap::new();
    for i in 0..8 {
        let out1 = e1.step(&wiggle(i));
        let out2 = e2.step(&wiggle(i));
        assert_eq!(out1.obs, out2.obs, "same seed ⇒ same obs");
        let f1 = e1.take_frames();
        let f2 = e2.take_frames();
        assert_eq!(f1.len(), f2.len(), "same seed ⇒ same frame count");
        assert_eq!(f1, f2, "spec-camera frames must be bit-equal");
        // two cameras ⇒ two frames per tick
        assert_eq!(f1.len(), 2, "one frame per camera per tick");
        for (_t, frame) in &f1 {
            let e = per_cam.entry(frame.cam.clone()).or_insert((0, 0));
            if let Some(png) = &frame.rgb_png {
                assert_eq!(&png[..8], &PNG_MAGIC, "{} rgb is a PNG", frame.cam);
                e.0 += 1;
            }
            if let Some(png) = &frame.depth_png {
                assert_eq!(&png[..8], &PNG_MAGIC, "{} depth is a PNG", frame.cam);
                // 16-bit grayscale IHDR: bit depth (24) = 16, colour (25) = 0
                assert_eq!(&png[24..26], &[16, 0], "depth bit depth");
                e.1 += 1;
            }
        }
    }
    // wrist: RGB + depth; top: RGB only (per the demo spec channels)
    assert_eq!(per_cam.get("wrist"), Some(&(8, 8)), "{per_cam:?}");
    assert_eq!(per_cam.get("top"), Some(&(8, 0)), "{per_cam:?}");
}

#[test]
fn wrist_extrinsics_follow_the_ee_tick_by_tick() {
    let spec = demo_spec();
    let wrist_spec = spec
        .scene
        .cameras
        .iter()
        .find(|c| c.name == "wrist")
        .expect("demo has a wrist camera")
        .clone();
    assert!(matches!(wrist_spec.pinhole().attach, CamAttach::Ee));
    let mut env = cam_env();
    env.reset(5);
    let (p0, l0, _) = env.cam_axes(&wrist_spec);
    for i in 0..20 {
        env.step(&wiggle(i));
    }
    let (p1, l1, _) = env.cam_axes(&wrist_spec);
    let dist = ((p1[0] - p0[0]).powi(2) + (p1[1] - p0[1]).powi(2) + (p1[2] - p0[2]).powi(2)).sqrt();
    assert!(
        dist > 0.005,
        "EE mount must move with the arm (moved {dist} m)"
    );
    // the mount offset is rigid: look_at keeps its offset from pos
    let d0 = [l0[0] - p0[0], l0[1] - p0[1], l0[2] - p0[2]];
    let d1 = [l1[0] - p1[0], l1[1] - p1[1], l1[2] - p1[2]];
    let n0 = (d0[0] * d0[0] + d0[1] * d0[1] + d0[2] * d0[2]).sqrt();
    let n1 = (d1[0] * d1[0] + d1[1] * d1[1] + d1[2] * d1[2]).sqrt();
    assert!(
        (n0 - n1).abs() < 1e-5,
        "rigid mount: |look_at − pos| invariant ({n0} vs {n1})"
    );
    // a world-mounted camera does NOT move
    let top = spec.scene.cameras.iter().find(|c| c.name == "top").unwrap();
    let (tp0, _, _) = env.cam_axes(top);
    env.step(&wiggle(99));
    let (tp1, _, _) = env.cam_axes(top);
    assert_eq!(tp0, tp1, "world mount is fixed");
}

/// The calibration gate, integration form: after a reset (plus a few
/// settled steps) the projected cube centre lands on cube-coloured
/// pixels in the top camera, and the projected tip lands on
/// arm-coloured pixels in the wrist camera — < 2 px, through the lens
/// model (k1 = 0.15 on the wrist).
#[test]
fn reprojection_gate_under_2px() {
    let mut env = cam_env();
    let _ = env.reset(1000);
    let layout = env.obs_layout().clone();
    let ee_i = layout.index_of("ee.x").expect("ee.x");
    let obj_i = layout.index_of("obj_rel.x").expect("obj_rel.x");
    let goal_i = layout.index_of("goal_rel.x").expect("goal_rel.x");

    const TIP_COLOR: [u8; 3] = [91, 141, 239];
    const CUBE_COLOR: [u8; 3] = [255, 206, 110];
    const GOAL_COLOR: [u8; 3] = [64, 200, 132];

    // probe two postures (home + yaw-swept): a single tick can have an
    // honest occlusion (the arm plane hovers over the goal — goal z ≈ 0
    // is the arm's own operating plane), the gate is the best sighting
    // over the probe set — the example's min-over-episode is the same
    // idea at full scale
    let mut sighted = std::collections::BTreeSet::<(String, &'static str)>::new();
    for action in [
        vec![0.0f32, 0.0, 0.0, 0.0, 0.0],
        vec![1.2, 0.3, -0.6, 0.4, 0.0],
    ] {
        for _ in 0..12 {
            env.step(&action);
        }
        let obs = env.step(&[0.0; 5]).obs;
        let tip = [obs[ee_i], obs[ee_i + 1], obs[ee_i + 2]];
        let cube = [
            tip[0] + obs[obj_i],
            tip[1] + obs[obj_i + 1],
            tip[2] + obs[obj_i + 2],
        ];
        let goal = [
            tip[0] + obs[goal_i],
            tip[1] + obs[goal_i + 1],
            tip[2] + obs[goal_i + 2],
        ];
        let targets: [(&str, [f32; 3], [u8; 3], bool); 3] = [
            ("tip", tip, TIP_COLOR, false),
            ("cube", cube, CUBE_COLOR, true),
            ("goal", goal, GOAL_COLOR, true),
        ];
        for shot in env.render_cameras() {
            let rgb = shot.rgb.as_deref().expect("rgb channels on");
            let depth = shot.depth.as_deref();
            for (name, point, color, depth_check) in targets {
                let Some((u, v, z)) = shot.pinhole.project(point) else {
                    continue; // behind the near plane (wrist-mounted normal)
                };
                if !(0.0..shot.w as f32).contains(&u) || !(0.0..shot.h as f32).contains(&v) {
                    continue; // out of frame — the example reports these
                }
                let d = if depth_check { depth } else { None };
                let err = nearest_color(rgb, shot.w, shot.h, (u, v), color, d, z);
                if err.is_finite() {
                    assert!(
                        err < 2.0,
                        "{} × {}: reprojection error {err:.2} px must be < 2 (u={u:.1} v={v:.1})",
                        shot.cam,
                        name
                    );
                    sighted.insert((shot.cam.clone(), name));
                }
            }
        }
    }
    // visibility bookkeeping: the overhead pinhole must have seen all
    // three targets across the two postures, the wrist camera its tip
    for cam in ["top", "wrist"] {
        let need: &[&str] = if cam == "top" {
            &["tip", "cube", "goal"]
        } else {
            &["tip"]
        };
        for n in need {
            assert!(
                sighted.contains(&(cam.to_string(), *n)),
                "{cam} × {n} never sighted (occluded/out-of-frame at every probe posture)"
            );
        }
    }
}

/// Nearest shade-invariant colour-matched pixel (flat shading scales a
/// surface's colour by one scalar — fit `pixel ≈ a·target`, accept the
/// small residual) to (u, v) within a ±4 px window whose depth agrees
/// with the point's camera-frame depth. The error in px.
fn nearest_color(
    rgb: &[u8],
    w: u32,
    h: u32,
    proj: (f32, f32),
    color: [u8; 3],
    depth: Option<&[u16]>,
    z: f32,
) -> f32 {
    let (u, v) = proj;
    let (px, py) = (u.round() as i32, v.round() as i32);
    let at = |x: i32, y: i32| {
        let i = (y as usize * w as usize + x as usize) * 3;
        [rgb[i], rgb[i + 1], rgb[i + 2]]
    };
    let matches = |c: [u8; 3]| {
        let p = [c[0] as f32, c[1] as f32, c[2] as f32];
        let t = [color[0] as f32, color[1] as f32, color[2] as f32];
        let a =
            (p[0] * t[0] + p[1] * t[1] + p[2] * t[2]) / (t[0] * t[0] + t[1] * t[1] + t[2] * t[2]);
        let e = [p[0] - a * t[0], p[1] - a * t[1], p[2] - a * t[2]];
        e[0] * e[0] + e[1] * e[1] + e[2] * e[2] <= 40.0
    };
    let mut best = f32::INFINITY;
    for dy in -4i32..=4 {
        for dx in -4i32..=4 {
            let (x, y) = (px + dx, py + dy);
            if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                continue;
            }
            if !matches(at(x, y)) {
                continue;
            }
            if let Some(dbuf) = depth {
                let mm = dbuf[y as usize * w as usize + x as usize] as f32;
                if (mm - z * 1000.0).abs() > 45.0 {
                    continue;
                }
            }
            best = best.min(((dx * dx + dy * dy) as f32).sqrt());
        }
    }
    best
}

#[test]
fn depth_channel_reports_millimetre_ranges() {
    let mut env = cam_env();
    env.reset(3);
    for shot in env.render_cameras() {
        let Some(depth) = &shot.depth else { continue };
        let hit: Vec<u32> = depth
            .iter()
            .filter(|d| **d > 0)
            .map(|d| *d as u32)
            .collect();
        assert!(!hit.is_empty(), "{} must see something", shot.cam);
        let max = *hit.iter().max().unwrap();
        assert!(max <= 65_000, "the bench lives under 65 m ({max} mm)");
        if shot.cam == "top" {
            // the camera axis hits the table 600 mm below
            let centre = depth[(120 * 320 + 160) as usize] as i32;
            assert!((centre - 600).abs() <= 5, "table depth {centre} mm");
        }
    }
}

#[test]
fn multi_track_dataset_round_trips_through_lerobot() {
    let spec = demo_spec();
    let mut ds = LeRobotV21::new(60.0, "t", vec!["s".into()], vec!["a".into()]);
    for c in &spec.scene.cameras {
        let p = c.pinhole();
        ds.add_camera(CameraTrack {
            name: c.name.clone(),
            w: p.width,
            h: p.height,
            fps: 60.0,
            depth: p.channels.depth,
        })
        .expect("unique names");
    }
    ds.start_episode("pick up the cube");
    ds.push_step(&[0.0], &[0.0]).unwrap();
    for (name, w, h) in [("wrist", 160u32, 120u32), ("top", 320, 240)] {
        let png =
            oriera_subsys::camera::encode_png(&vec![7u8; (w * h * 3) as usize], w, h).unwrap();
        ds.push_frame(oriera_embodied::embodied_env::CameraFrame {
            cam: name.into(),
            rgb_png: Some(png),
            depth_png: if name == "wrist" {
                Some(
                    oriera_subsys::camera::encode_depth_png(&vec![600u16; (w * h) as usize], w, h)
                        .unwrap(),
                )
            } else {
                None
            },
        })
        .unwrap();
    }
    ds.finish_episode();
    let root_pre = std::env::temp_dir().join(format!("oriera-cam-it-info-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root_pre);
    ds.write_dir(&root_pre).expect("tree for info.json");
    let info = std::fs::read_to_string(root_pre.join("meta/info.json")).expect("info.json");
    let _ = std::fs::remove_dir_all(&root_pre);
    assert!(info.contains("\"observation.images.wrist\""), "{info}");
    assert!(info.contains("\"depth.wrist\""), "{info}");
    let root = std::env::temp_dir().join(format!("oriera-cam-it-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    ds.write_dir(&root).expect("v2.1 tree");
    assert!(root
        .join("videos/chunk-000/depth.wrist/episode_000000/frame_000000.png")
        .exists());
    let root_v3 = root.join("v3");
    ds.write_dir_v3(&root_v3).expect("v3 tree");
    assert!(root_v3
        .join("videos/depth.wrist/chunk-000/file-000/frame_000000.png")
        .exists());
    let _ = std::fs::remove_dir_all(&root);
}

/// `PinholeCam` construction from the spec side: the demo's two
/// cameras build, project the bench centre near the principal point,
/// and reject degenerate extrinsics.
#[test]
fn pinhole_from_spec_geometry() {
    let spec = demo_spec();
    for c in &spec.scene.cameras {
        let p = c.pinhole();
        let cam = PinholeCam::from_look(
            p.pos,
            p.look_at,
            p.up,
            p.width,
            p.height,
            p.fx,
            p.fy,
            p.cx,
            p.cy,
            oriera_subsys::camera::Distortion {
                k1: p.dist.k1,
                k2: p.dist.k2,
                p1: p.dist.p1,
                p2: p.dist.p2,
            },
            p.channels.rgb,
            p.channels.depth,
        )
        .expect("demo cameras are valid");
        // the look_at point projects (near) the principal point
        let look_at = [
            p.pos[0] + (p.look_at[0] - p.pos[0]),
            p.pos[1] + (p.look_at[1] - p.pos[1]),
            p.pos[2] + (p.look_at[2] - p.pos[2]),
        ];
        let (u, v, _) = cam.project(look_at).expect("look_at in front");
        assert!(
            (u - p.cx).abs() < 1e-3 && (v - p.cy).abs() < 1e-3,
            "optical axis lands on the principal point ({u} {v})"
        );
    }
}

/// P2-R render guard (PLAN-embodied-v3 P2 深水区 Wave-1 · R 包): every
/// prop — `props[1..]` included — and every fixture standing on the
/// table must reach BOTH CPU camera tracks at its live world pose.
/// Pre-fix probes (caa207c) on these presets found two broken entries:
///
/// * pinhole (`render_pinhole` via `scene_view`): stack's base B
///   (`props[1]`) hidden under the goal disc, which floated at the
///   mid-air goal "A's centre on top of B" (y 0.577 > B's top 0.55);
/// * legacy `--cam` top track: only the table and `props[0]` were ever
///   drawn — B, the bin's double walls (`fixtures[2..9]`), the legacy
///   5-prop bin's walls (`props[1..4]`) and the drawer-v2 cabinet missing.
///
/// Probe: an overhead calibrated camera; per object, the centre of its
/// top face must show the object's own colour AND win the z-buffer
/// (depth = the top face's camera depth — not hidden, not drawn at the
/// origin); in the top track the object must own its pixels.
mod p2r_props_render {
    use super::nearest_color;
    use oriera_embodied::embodied_env::{
        CamAttach, CamChannels, CameraKind, CameraSpec, DistortionSpec, EmbodiedEnv, PinholeSpec,
        TableTopEnv,
    };
    use oriera_embodied::spec::SceneSpec;
    use oriera_subsys::camera::{encode_png, CameraView};
    use oriera_subsys::rigid3d::PropShape;

    const PROP_COLOR: [u8; 3] = [255, 206, 110];
    /// `scene_view`'s colour for every non-ground fixture (the table
    /// shares it — the depth check is what separates a wall from it).
    const FIXTURE_COLOR: [u8; 3] = [56, 68, 88];
    /// The legacy top track's furniture colour (`fixtures[2..]`).
    const TOP_FURNITURE: [u8; 3] = [112, 126, 154];

    fn load(rel: &str) -> SceneSpec {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
        SceneSpec::load_file(&path)
            .unwrap_or_else(|e| panic!("{rel}: {e}"))
            .spec
    }

    /// Straight-down world camera over the whole work area, RGB + depth.
    fn overhead() -> CameraSpec {
        CameraSpec {
            name: "guard_top".into(),
            kind: CameraKind::Pinhole(PinholeSpec {
                width: 320,
                height: 240,
                fx: 300.0,
                fy: 300.0,
                cx: 160.0,
                cy: 120.0,
                dist: DistortionSpec::default(),
                pos: [0.20, 1.20, 0.0],
                look_at: [0.20, 0.50, 0.0],
                up: [0.0, 0.0, 1.0],
                channels: CamChannels {
                    rgb: true,
                    depth: true,
                },
                attach: CamAttach::World,
            }),
        }
    }

    /// `rel` reset at seed 1000 with the overhead camera mounted and the
    /// arm swung to the −X side (base yaw → π) so no link hangs over the
    /// work area — occlusion by the arm is not what this guard measures.
    fn swung_env(rel: &str) -> TableTopEnv {
        let mut env = load(rel).to_env();
        env.set_cameras(vec![overhead()]);
        env.reset(1000);
        for _ in 0..240 {
            if env.ee_pos()[0] < -0.30 {
                break;
            }
            env.step(&[3.0, 0.0, 0.0, 0.0, 0.0]);
        }
        assert!(
            env.ee_pos()[0] < -0.30,
            "{rel}: the arm never cleared the work area (tip {:?})",
            env.ee_pos()
        );
        env
    }

    fn rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
        let u = [q[0], q[1], q[2]];
        let cross = |a: [f32; 3], b: [f32; 3]| {
            [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ]
        };
        let t = cross(u, v);
        let t = [2.0 * t[0], 2.0 * t[1], 2.0 * t[2]];
        let c = cross(u, t);
        [
            v[0] + q[3] * t[0] + c[0],
            v[1] + q[3] * t[1] + c[1],
            v[2] + q[3] * t[2] + c[2],
        ]
    }

    /// The legacy top view's pixel of a world point (`CameraView::px`).
    fn top_px(view: &CameraView, p: [f32; 3]) -> (i32, i32) {
        let x = ((p[0] - (view.center[0] - view.extent[0] / 2.0)) / view.extent[0] * view.w as f32)
            .round() as i32;
        let y = ((p[2] - (view.center[1] - view.extent[1] / 2.0)) / view.extent[1] * view.h as f32)
            .round() as i32;
        (x, y)
    }

    fn rgb_at(rgb: &[u8], w: u32, (x, y): (i32, i32)) -> [u8; 3] {
        let i = (y as usize * w as usize + x as usize) * 3;
        [rgb[i], rgb[i + 1], rgb[i + 2]]
    }

    /// Check every prop and every raised fixture on both CPU tracks;
    /// returns how many objects were checked.
    fn assert_scene_on_both_cpu_tracks(name: &str, env: &TableTopEnv) -> usize {
        let shots = env.render_cameras();
        let shot = &shots[0];
        let rgb = shot.rgb.as_deref().expect("rgb on");
        let depth = shot.depth.as_deref().expect("depth on");
        let view = CameraView::default();
        let top = env.render_top(&view);
        let table_top = env
            .scene
            .fixtures
            .get(1)
            .map_or(0.5, |f| f.center[1] + f.half[1]);
        // pinhole: a colour-matched pixel within 1 px of the top-face
        // centre (shared `nearest_color`, depth-filtered) AND the z-buffer
        // at the centre holding that face's own depth (front-most)
        let pinhole = |what: &str, top_face: [f32; 3], color: [u8; 3]| {
            let (u, v, z) = shot
                .pinhole
                .project(top_face)
                .unwrap_or_else(|| panic!("{name} {what}: behind the camera"));
            assert!(
                (0.0..shot.w as f32).contains(&u) && (0.0..shot.h as f32).contains(&v),
                "{name} {what}: out of frame (u={u:.1} v={v:.1})"
            );
            let err = nearest_color(rgb, shot.w, shot.h, (u, v), color, Some(depth), z);
            let (x, y) = (u.round() as i32, v.round() as i32);
            let d = depth[y as usize * shot.w as usize + x as usize];
            assert!(
                err <= 1.0,
                "{name} {what}: not the front-most {color:?} surface at its top face \
                 (px ({x},{y}) = {:?}, depth {d} mm vs top face {:.0} mm) — hidden or drawn elsewhere",
                rgb_at(rgb, shot.w, (x, y)),
                z * 1000.0
            );
            assert!(
                (d as f32 - z * 1000.0).abs() <= 3.0,
                "{name} {what}: z-buffer holds {d} mm at its top-face centre, the face is at {:.0} mm",
                z * 1000.0
            );
        };
        let mut checked = 0;
        for (i, p) in env.scene.props.iter().enumerate() {
            let what = format!("props[{i}] {}", p.name);
            let pose = env
                .prop_pose(i)
                .unwrap_or_else(|| panic!("{name} {what}: no pose"));
            let h = match p.shape {
                PropShape::Cuboid { half } => half[1],
                PropShape::Cylinder { half_h, .. } => half_h,
                PropShape::Sphere { r } => r,
            };
            let up = rotate(pose.rot, [0.0, h, 0.0]);
            let face = [
                pose.pos[0] + up[0],
                pose.pos[1] + up[1],
                pose.pos[2] + up[2],
            ];
            pinhole(&what, face, PROP_COLOR);
            let px = top_px(&view, pose.pos);
            assert_eq!(
                rgb_at(&top, view.w, px),
                PROP_COLOR,
                "{name} {what}: missing from the legacy top track at px {px:?}"
            );
            checked += 1;
        }
        for (i, f) in env.scene.fixtures.iter().enumerate().skip(2) {
            let what = format!("fixtures[{i}] {}", f.name);
            let top_y = f.center[1] + f.half[1];
            if top_y <= table_top + 1e-3 {
                // flush with the table (the bin floor): coplanar, same
                // colour — the table wins the tie in both tracks
                continue;
            }
            pinhole(&what, [f.center[0], top_y, f.center[2]], FIXTURE_COLOR);
            // top track: the slab owns the majority of its footprint (the
            // goal disc / props legitimately paint over a few pixels)
            let (cx, cy) = top_px(&view, f.center);
            let hw = (f.half[0] / view.extent[0] * view.w as f32).round() as i32;
            let hh = (f.half[2] / view.extent[1] * view.h as f32).round() as i32;
            let (mut own, mut all) = (0, 0);
            for y in (cy - hh)..=(cy + hh) {
                for x in (cx - hw)..=(cx + hw) {
                    all += 1;
                    if rgb_at(&top, view.w, (x, y)) == TOP_FURNITURE {
                        own += 1;
                    }
                }
            }
            assert!(
                own * 2 > all,
                "{name} {what}: missing from the legacy top track ({own}/{all} footprint px)"
            );
            checked += 1;
        }
        checked
    }

    #[test]
    fn stack_base_b_reaches_both_cpu_camera_tracks() {
        let env = swung_env("specs/official/stack-4dof.json");
        assert_eq!(
            assert_scene_on_both_cpu_tracks("stack-4dof", &env),
            2,
            "block A + base B (props[1])"
        );
    }

    #[test]
    fn bin_walls_reach_both_cpu_camera_tracks() {
        // P2 bin: cube + the 8 wall slabs fixtures[2..9] (floor is flush)
        let env = swung_env("specs/official/bin-pick-4dof.json");
        assert_eq!(assert_scene_on_both_cpu_tracks("bin-pick-4dof", &env), 9);
        // the M2 approximation (archive 14fc9bf8): walls are props[1..4]
        let env = swung_env("tests/fixtures/bin-pick-4dof-legacy-5prop.json");
        assert_eq!(assert_scene_on_both_cpu_tracks("bin-legacy-5prop", &env), 5);
    }

    #[test]
    fn drawer_v2_panel_and_cabinet_reach_both_cpu_camera_tracks() {
        // slide panel props[0] (non-default 120 mm cuboid) + back + rails
        let env = swung_env("specs/official/drawer-4dof-v2.json");
        assert_eq!(assert_scene_on_both_cpu_tracks("drawer-4dof-v2", &env), 4);
    }

    /// `render_top` is exactly the `top` track the capture path exports
    /// — on the legacy bench (the byte-frozen `capture_png` path) and on
    /// a completed scene (stack) alike.
    #[test]
    fn render_top_is_the_exported_top_track() {
        for rel in [
            "specs/official/tabletop-4dof-pick.json",
            "specs/official/stack-4dof.json",
        ] {
            let mut env = load(rel).to_env();
            let view = CameraView::default();
            env.set_camera(Some(view.clone()));
            env.reset(1000);
            for i in 0..3 {
                let expect = encode_png(&env.render_top(&view), view.w, view.h).unwrap();
                env.step(&[0.1 * i as f32, 0.2, -0.3, 0.1, 0.0]);
                let frames = env.take_frames();
                let top = frames
                    .iter()
                    .find(|(_, f)| f.cam == "top")
                    .expect("top frame");
                assert_eq!(
                    top.1.rgb_png.as_deref(),
                    Some(expect.as_slice()),
                    "{rel} step {i}: render_top ≠ the exported top track"
                );
            }
        }
    }

    /// The goal marker is a floor decal: a surface goal comes back
    /// verbatim (the golden bench keeps its marker bytes), a mid-air goal
    /// drops onto the table top beneath it — XZ always the goal's.
    #[test]
    fn goal_marker_rests_on_the_surface_under_the_goal() {
        for (rel, dropped_to) in [
            ("specs/official/tabletop-4dof-pick.json", None),
            ("specs/official/mobile-arm-4dof.json", None),
            ("specs/official/stack-4dof.json", Some(0.5f32)),
            ("specs/official/drawer-4dof-v2.json", Some(0.5f32)),
        ] {
            let mut env = load(rel).to_env();
            env.reset(1000);
            let (g, m) = (env.goal(), env.goal_marker());
            assert_eq!(
                [m[0], m[2]],
                [g[0], g[2]],
                "{rel}: marker keeps the goal XZ"
            );
            match dropped_to {
                None => assert_eq!(m, g, "{rel}: a surface goal comes back verbatim"),
                Some(y) => assert!(
                    m[1] == y && g[1] > y + 0.01,
                    "{rel}: mid-air goal {g:?} must drop onto the table top {y} (got {m:?})"
                ),
            }
        }
    }
}
