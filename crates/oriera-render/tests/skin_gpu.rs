#![cfg(feature = "render-wgpu")]
//! #153 M1 CPU-GPU 互证:GPU `vs_skinned`(Σ w·M·p)与 CPU 参考蒙皮同公式,
//! 同相机同面板渲染 CesiumMan(CC-BY 4.0 夹具,复用 oriera-asset tests
//! fixtures,不重复入库),像素级对拍——互证门槛与 splat M1 同域:
//! 99.9% 像素 ≤2/255 且 RMS ≤1/255。另验:动画两时刻画面显著不同
//! (面板真驱动变形,非静帧)、面板越界/长度不匹配显式拒绝。

use oriera_asset::glb::{decode_glb_rig, joint_matrices_at, GlbRig};
use oriera_render::camera::Camera;
use oriera_render::gpu::{HeadlessRenderer, RenderedImage, ScenePart, SkinnedPart};
use std::path::PathBuf;

fn cesiumman() -> GlbRig {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../oriera-asset/tests/fixtures/CesiumMan.glb");
    decode_glb_rig(&std::fs::read(path).expect("fixture CesiumMan.glb missing"))
        .expect("CesiumMan must decode")
}

/// 一次绘制的自有数据(joints u16→u32 摊平 + CPU 参考蒙皮结果)。
/// 持有它才能同时借出 ScenePart(蒙皮后)与 SkinnedPart(bind 空间)。
struct RigDraw {
    joints32: Vec<[u32; 4]>,
    cpu_pos: Vec<[f32; 3]>,
    cpu_nrm: Vec<[f32; 3]>,
}

/// CPU 参考蒙皮:与 WGSL `vs_skinned` 逐项同公式(Σ w·M·p、法线同 mat4 的
/// (n,0) 后归一)。positions/normals 均 bind 空间进、世界空间出。
fn skin_cpu(
    positions: &[[f32; 3]],
    normals: Option<&[[f32; 3]]>,
    joints: &[[u32; 4]],
    weights: &[[f32; 4]],
    palette: &[[[f32; 4]; 4]],
) -> (Vec<[f32; 3]>, Vec<[f32; 3]>) {
    let identity = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    let xf = |m: &[[f32; 4]; 4], v: [f32; 3], w: f32| -> [f32; 3] {
        [
            w * (m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2] + m[0][3]),
            w * (m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2] + m[1][3]),
            w * (m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2] + m[2][3]),
        ]
    };
    // 法线变换:同 mat4 但 (n,0)——**无平移项**(与 vs_skinned 的
    // `m * vec4f(n, 0.0)` 同式;平移进法线是错误实现,互证第一轮逮住的
    // 正是本测试初版的这个 bug)。
    let xf_n = |m: &[[f32; 4]; 4], v: [f32; 3], w: f32| -> [f32; 3] {
        [
            w * (m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2]),
            w * (m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2]),
            w * (m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2]),
        ]
    };
    let mut pos_out = Vec::with_capacity(positions.len());
    let mut nrm_out = vec![[0.0, 0.0, 1.0]; positions.len()];
    for (i, p) in positions.iter().enumerate() {
        let mut acc = [0.0f32; 3];
        let mut acc_n = [0.0f32; 3];
        let n = normals
            .and_then(|ns| ns.get(i).copied())
            .unwrap_or([0.0, 0.0, 1.0]);
        for k in 0..4 {
            let wt = weights[i][k];
            if wt <= 0.0 {
                continue;
            }
            let m = palette.get(joints[i][k] as usize).unwrap_or(&identity);
            let tp = xf(m, *p, wt);
            let tn = xf_n(m, n, wt);
            for (r, (tp_r, tn_r)) in tp.iter().zip(&tn).enumerate() {
                acc[r] += *tp_r;
                acc_n[r] += *tn_r;
            }
        }
        let len = (acc_n[0] * acc_n[0] + acc_n[1] * acc_n[1] + acc_n[2] * acc_n[2]).sqrt();
        if len > f32::EPSILON {
            for v in &mut acc_n {
                *v /= len;
            }
        }
        pos_out.push(acc);
        nrm_out[i] = acc_n;
    }
    (pos_out, nrm_out)
}

fn build_draw(rig: &GlbRig, palette: &[[[f32; 4]; 4]]) -> RigDraw {
    let prim = &rig.primitives[0];
    let joints32: Vec<[u32; 4]> = prim
        .joints
        .iter()
        .map(|j| [j[0] as u32, j[1] as u32, j[2] as u32, j[3] as u32])
        .collect();
    let (cpu_pos, cpu_nrm) = skin_cpu(
        &prim.positions,
        prim.normals.as_deref(),
        &joints32,
        &prim.weights,
        palette,
    );
    RigDraw {
        joints32,
        cpu_pos,
        cpu_nrm,
    }
}

fn reference_part<'a>(rig: &'a GlbRig, d: &'a RigDraw) -> ScenePart<'a> {
    let prim = &rig.primitives[0];
    ScenePart {
        positions: &d.cpu_pos,
        normals: Some(&d.cpu_nrm),
        uvs: None,
        indices: &prim.indices,
        base_color: prim.base_color.unwrap_or([1.0, 1.0, 1.0, 1.0]),
        metallic: 0.0,
        roughness: 0.9,
        base_texture: None,
        mr_texture: None,
    }
}

fn skinned_part<'a>(
    rig: &'a GlbRig,
    d: &'a RigDraw,
    palette: &'a [[[f32; 4]; 4]],
) -> SkinnedPart<'a> {
    let prim = &rig.primitives[0];
    SkinnedPart {
        positions: &prim.positions,
        normals: prim.normals.as_deref(),
        uvs: None,
        indices: &prim.indices,
        base_color: prim.base_color.unwrap_or([1.0, 1.0, 1.0, 1.0]),
        metallic: 0.0,
        roughness: 0.9,
        base_texture: None,
        mr_texture: None,
        joints: &d.joints32,
        weights: &prim.weights,
        palette,
    }
}

/// 相机:由蒙皮后包围盒自动取景(CesiumMan bind/动画位姿都稳)。
fn camera_for(positions: &[[f32; 3]]) -> Camera {
    let mut min = [f32::MAX; 3];
    let mut max = [f32::MIN; 3];
    for p in positions {
        for r in 0..3 {
            min[r] = min[r].min(p[r]);
            max[r] = max[r].max(p[r]);
        }
    }
    let center = [
        (min[0] + max[0]) * 0.5,
        (min[1] + max[1]) * 0.5,
        (min[2] + max[2]) * 0.5,
    ];
    let radius = (max[0] - min[0])
        .max(max[1] - min[1])
        .max(max[2] - min[2])
        .max(0.01);
    Camera::look_at(
        [
            center[0] + radius * 2.0,
            center[1] + radius * 0.6,
            center[2] + radius * 2.6,
        ],
        center,
        [0.0, 1.0, 0.0],
        40.0,
        320,
        240,
        0.05,
        100.0,
    )
}

/// 像素对拍(分级互证):几何由 CPU/GPU 各自 f32 计算,~1e-5 位置差在
/// 剪影边缘翻转栅格化归属(亚像素抖动),这是 inherent 的;真正锁定数学的
/// 是**内部着色像素**。分级:
/// - both-bg / both-object:严格域(both-bg 须一致;both-object 99.9% ≤2/255
///   且 RMS ≤1——着色数学互证);
/// - mixed(一 bg 一 object,剪影环):有界放行 ≤3% 像素。
fn compare(a: &RenderedImage, b: &RenderedImage) -> Result<(usize, f64, usize), String> {
    assert_eq!((a.width, a.height), (b.width, b.height));
    // 背景哨兵:CLEAR #0d1421 → BGRA(33,20,13,255);命中判定按容差 2
    let is_bg = |px: [u8; 4]| (33i32 - px[0] as i32).abs() <= 2;
    let n = a.width as usize * a.height as usize;
    let (mut both_obj_sum2, mut both_obj_n, mut both_obj_over) = (0.0f64, 0usize, 0usize);
    let mut mixed_px = 0usize;
    let mut strict_fail = Vec::new();
    for y in 0..a.height {
        for x in 0..a.width {
            let pa = a.pixel(x, y);
            let pb = b.pixel(x, y);
            let (ba, bb) = (is_bg(pa), is_bg(pb));
            if ba != bb {
                mixed_px += 1;
                continue;
            }
            if ba {
                if pa != pb {
                    strict_fail.push((x, y, "bg mismatch"));
                }
                continue;
            }
            let mut maxd = 0i32;
            for k in 0..3 {
                let d = (pa[k] as i32 - pb[k] as i32).abs();
                sum_add(&mut both_obj_sum2, d);
                maxd = maxd.max(d);
            }
            both_obj_n += 3;
            if maxd > 2 {
                both_obj_over += 1;
                if strict_fail.len() < 8 {
                    strict_fail.push((x, y, "object diff"));
                }
            }
        }
    }
    fn sum_add(acc: &mut f64, d: i32) {
        *acc += (d * d) as f64;
    }
    let rms = (both_obj_sum2 / both_obj_n.max(1) as f64).sqrt();
    let total_px = n as f64;
    if !strict_fail.is_empty() && both_obj_over as f64 > both_obj_n as f64 * 0.001 {
        return Err(format!(
            "内部着色超差: {}/{} 通道 >2/255({:.2}% 允 0.1%),RMS={rms:.3};样本 {:?}",
            both_obj_over,
            both_obj_n,
            both_obj_over as f64 / both_obj_n as f64 * 100.0,
            &strict_fail[..strict_fail.len().min(4)]
        ));
    }
    if both_obj_over as f64 > both_obj_n as f64 * 0.001 || rms > 1.0 {
        return Err(format!(
            "内部着色互证超差: over={both_obj_over} rms={rms:.3}"
        ));
    }
    if mixed_px as f64 > total_px * 0.03 {
        return Err(format!(
            "剪影环超界: mixed={mixed_px}({:.2}% 允 3%)",
            mixed_px as f64 / total_px * 100.0
        ));
    }
    Ok((both_obj_over, rms, mixed_px))
}

#[test]
fn cpu_gpu_skinned_frames_agree() {
    let rig = cesiumman();
    let dur = rig.animations[0].duration;
    let renderer = HeadlessRenderer::new().expect("gpu init");

    for frac in [0.0f32, 0.25, 0.5, 0.75] {
        let t = dur * frac;
        let palettes = joint_matrices_at(&rig, Some(0), t);
        let draw = build_draw(&rig, &palettes[0]);
        let camera = camera_for(&draw.cpu_pos);

        let reference = reference_part(&rig, &draw);
        let skinned = skinned_part(&rig, &draw, &palettes[0]);
        let a = renderer
            .render_scene_views(&[reference], &camera)
            .expect("cpu path");
        let b = renderer
            .render_skinned_views(&[skinned], &camera)
            .expect("gpu path");
        let (over, rms, mixed) = compare(&a, &b).unwrap_or_else(|e| panic!("t={frac}: {e}"));
        assert!(
            over == 0 && rms <= 0.5 && mixed < 2000,
            "t={frac}: 分级指标异常 over={over} rms={rms:.3} mixed={mixed}"
        );
    }
}

#[test]
fn animation_drives_pixel_difference() {
    let rig = cesiumman();
    let dur = rig.animations[0].duration;
    let renderer = HeadlessRenderer::new().expect("gpu init");

    let render_at = |t: f32| {
        let palettes = joint_matrices_at(&rig, Some(0), t);
        let draw = build_draw(&rig, &palettes[0]);
        let camera = camera_for(&draw.cpu_pos);
        let skinned = skinned_part(&rig, &draw, &palettes[0]);
        renderer
            .render_skinned_views(&[skinned], &camera)
            .expect("gpu path")
    };
    let a = render_at(0.0);
    let b = render_at(dur * 0.5);
    // 全图粗对拍(不经分级):两姿态显著不同 = 动画真在动
    let n = (a.width as usize) * (a.height as usize);
    let mut changed_px = 0usize;
    for y in 0..a.height {
        for x in 0..a.width {
            let (pa, pb) = (a.pixel(x, y), b.pixel(x, y));
            if (0..3).any(|k| (pa[k] as i32 - pb[k] as i32).abs() > 2) {
                changed_px += 1;
            }
        }
    }
    assert!(
        changed_px * 100 > n && changed_px > 500,
        "两时刻画面应显著不同(动画在动):changed={changed_px}/{}({:.2}%,门 1%)",
        n,
        changed_px as f64 / n as f64 * 100.0
    );
}

#[test]
fn skinned_path_validates_palette_and_lengths() {
    let rig = cesiumman();
    let renderer = HeadlessRenderer::new().expect("gpu init");
    let palettes = joint_matrices_at(&rig, Some(0), 0.0);
    let camera = camera_for(&rig.primitives[0].positions);

    // 顶点关节索引越界面板 → 显式拒绝(WGSL 越界未定义,CPU 侧守门)
    let draw = build_draw(&rig, &palettes[0]);
    let bad_joints = vec![[99u32; 4]; rig.primitives[0].positions.len()];
    let skinned = SkinnedPart {
        joints: &bad_joints,
        ..skinned_part(&rig, &draw, &palettes[0])
    };
    let err = renderer
        .render_skinned_views(&[skinned], &camera)
        .expect_err("OOB joint index must be rejected");
    assert!(format!("{err:?}").contains("out of palette bounds"));

    // 长度不匹配 → 显式拒绝
    let draw = build_draw(&rig, &palettes[0]);
    let empty: Vec<[u32; 4]> = Vec::new();
    let skinned = SkinnedPart {
        joints: &empty,
        ..skinned_part(&rig, &draw, &palettes[0])
    };
    let err = renderer
        .render_skinned_views(&[skinned], &camera)
        .expect_err("length mismatch must be rejected");
    assert!(format!("{err:?}").contains("must match positions"));
}
