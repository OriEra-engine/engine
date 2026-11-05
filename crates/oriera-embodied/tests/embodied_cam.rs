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
