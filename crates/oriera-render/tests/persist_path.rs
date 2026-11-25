//! 持久渲染路径(RFC 0004 步 1a / 1b)的行为测试:断言 A1–A5 与异步回读 B1–B5。
//!
//! 需要本机有可用的 wgpu 适配器,按仓库惯例默认忽略。本地运行:
//!
//! ```sh
//! WGPU_BACKEND=gl EGL_PLATFORM=surfaceless \
//!   cargo test -p oriera-render --features render-wgpu --test persist_path -- --ignored
//! ```
#![cfg(feature = "render-wgpu")]

use oriera_render::camera::Camera;
use oriera_render::gpu::{
    DrawList, FrameStats, GpuMaterial, GpuMesh, HeadlessRenderer, RenderTarget, RenderedImage,
    ScenePart, DEFAULT_INSTANCE_CAPACITY, MAX_READS_IN_FLIGHT,
};

/// 画面清屏色的 BGRA 取整值(CLEAR = 0.05 / 0.08 / 0.13)。
const BACKGROUND: [u8; 4] = [33, 20, 13, 255];
const W: u32 = 320;
const H: u32 = 180;

fn renderer() -> HeadlessRenderer {
    HeadlessRenderer::new().expect("adapter")
}

fn camera() -> Camera {
    Camera::look_at(
        [0.0, 0.0, 90.0],
        [0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        60.0,
        W,
        H,
        0.1,
        500.0,
    )
}

fn identity() -> [[f32; 4]; 4] {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn translate(x: f32, y: f32, z: f32) -> [[f32; 4]; 4] {
    [
        [1.0, 0.0, 0.0, x],
        [0.0, 1.0, 0.0, y],
        [0.0, 0.0, 1.0, z],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn scale(s: f32) -> [[f32; 4]; 4] {
    [
        [s, 0.0, 0.0, 0.0],
        [0.0, s, 0.0, 0.0],
        [0.0, 0.0, s, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// 绕 Y 轴旋转(行主序)。
fn rotate_y(deg: f32) -> [[f32; 4]; 4] {
    let (s, c) = deg.to_radians().sin_cos();
    [
        [c, 0.0, s, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [-s, 0.0, c, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// 轴对齐长方体,中心在原点:24 顶点(每面独立法线)+ 36 索引。
fn box_mesh(hx: f32, hy: f32, hz: f32) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u32>) {
    let (x, y, z) = (hx, hy, hz);
    // 每项:四个角点 + 该面法线。
    let faces: [([[f32; 3]; 4], [f32; 3]); 6] = [
        // +X
        (
            [[x, -y, -z], [x, y, -z], [x, y, z], [x, -y, z]],
            [1.0, 0.0, 0.0],
        ),
        // -X
        (
            [[-x, -y, z], [-x, y, z], [-x, y, -z], [-x, -y, -z]],
            [-1.0, 0.0, 0.0],
        ),
        // +Y
        (
            [[-x, y, -z], [-x, y, z], [x, y, z], [x, y, -z]],
            [0.0, 1.0, 0.0],
        ),
        // -Y
        (
            [[-x, -y, z], [-x, -y, -z], [x, -y, -z], [x, -y, z]],
            [0.0, -1.0, 0.0],
        ),
        // +Z
        (
            [[-x, -y, z], [x, -y, z], [x, y, z], [-x, y, z]],
            [0.0, 0.0, 1.0],
        ),
        // -Z
        (
            [[x, -y, -z], [-x, -y, -z], [-x, y, -z], [x, y, -z]],
            [0.0, 0.0, -1.0],
        ),
    ];
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut indices = Vec::new();
    for (corners, n) in faces {
        let base = positions.len() as u32;
        positions.extend(corners);
        normals.extend([n; 4]);
        // 从外侧看逆时针:0-1-2、0-2-3。
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    (positions, normals, indices)
}

/// 立方体,半边长 `half`。
fn cube(half: f32) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u32>) {
    box_mesh(half, half, half)
}

fn upload(r: &HeadlessRenderer, mesh: &(Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u32>)) -> GpuMesh {
    r.upload_mesh(&mesh.0, Some(&mesh.1), None, &mesh.2)
        .expect("upload_mesh")
}

fn material(r: &HeadlessRenderer, color: [f32; 4]) -> GpuMaterial {
    r.create_material(color, 0.0, 0.8, None, None)
}

fn is_background(p: [u8; 4]) -> bool {
    (0..3).all(|i| p[i].abs_diff(BACKGROUND[i]) <= 1)
}

fn lit_pixels(img: &RenderedImage) -> usize {
    (0..img.height)
        .flat_map(|y| (0..img.width).map(move |x| (x, y)))
        .filter(|&(x, y)| !is_background(img.pixel(x, y)))
        .count()
}

/// 被照亮像素的质心(x, y)。
fn centroid(img: &RenderedImage) -> (f64, f64) {
    let (mut sx, mut sy, mut n) = (0u64, 0u64, 0u64);
    for y in 0..img.height {
        for x in 0..img.width {
            if !is_background(img.pixel(x, y)) {
                sx += u64::from(x);
                sy += u64::from(y);
                n += 1;
            }
        }
    }
    assert!(n > 0, "nothing lit");
    (sx as f64 / n as f64, sy as f64 / n as f64)
}

fn frame(
    r: &HeadlessRenderer,
    list: &DrawList<'_>,
    cam: &Camera,
    target: &RenderTarget,
) -> FrameStats {
    r.render_draw_list(list, cam, target)
        .expect("render_draw_list")
}

/// A1:同一网格、单位模型矩阵、同一材质,持久路径与快照路径像素等价。
/// 容差来自"材质因子从顶点插值改为 uniform"的浮点差异,预期实际为 0。
#[test]
#[ignore = "requires a local GPU adapter (none in CI)"]
fn persistent_path_matches_snapshot_path() {
    let r = renderer();
    let cam = camera();
    let (positions, normals, indices) = cube(6.0);
    let color = [0.6, 0.6, 0.62, 1.0];
    let snapshot = r
        .render_scene_views(
            &[ScenePart {
                positions: &positions,
                normals: Some(&normals),
                uvs: None,
                indices: &indices,
                base_color: color,
                metallic: 0.0,
                roughness: 0.8,
                base_texture: None,
                mr_texture: None,
            }],
            &cam,
        )
        .expect("snapshot");

    let mesh = upload(&r, &(positions, normals, indices));
    let mat = material(&r, color);
    let target = r.create_target(W, H).expect("target");
    let mut list = DrawList::new();
    list.push(&mesh, &mat, identity());
    frame(&r, &list, &cam, &target);
    let img = r.read_target(&target).expect("readback");

    assert_eq!((img.width, img.height), (W, H));
    let mut differing = 0usize;
    let mut max_diff = 0u8;
    for y in 0..H {
        for x in 0..W {
            let (a, b) = (snapshot.pixel(x, y), img.pixel(x, y));
            let d = (0..4).map(|i| a[i].abs_diff(b[i])).max().unwrap_or(0);
            if d > 0 {
                differing += 1;
                max_diff = max_diff.max(d);
            }
        }
    }
    let ratio = differing as f64 / f64::from(W * H);
    eprintln!("A1 differing={differing} ratio={ratio:.6} max_diff={max_diff}");
    assert!(
        ratio <= 0.001,
        "differing pixels {differing} (ratio {ratio:.6})"
    );
    assert!(max_diff <= 1, "max channel diff {max_diff}");
}

/// A2:预热后连续 100 帧(实例在动)不新建任何 GPU 资源。
#[test]
#[ignore = "requires a local GPU adapter (none in CI)"]
fn steady_frames_create_no_gpu_resources() {
    let r = renderer();
    let cam = camera();
    let mesh = upload(&r, &cube(1.0));
    let mat = material(&r, [0.8, 0.3, 0.2, 1.0]);
    let target = r.create_target(W, H).expect("target");
    let mut list = DrawList::new();
    for i in 0..64 {
        list.push(&mesh, &mat, translate(i as f32 - 32.0, 0.0, 0.0));
    }
    for f in 0..103 {
        // 每帧改一次模型矩阵,确认"内容变了"也不会触发任何重建。
        for (i, inst) in list.instances.iter_mut().enumerate() {
            inst.model = translate(i as f32 - 32.0, (f as f32 * 0.1).sin() * 10.0, 0.0);
        }
        let stats = frame(&r, &list, &cam, &target);
        if f >= 3 {
            assert_eq!(stats.created_buffers, 0, "frame {f}");
            assert_eq!(stats.created_bind_groups, 0, "frame {f}");
            assert_eq!(stats.created_textures, 0, "frame {f}");
        }
    }
    // 留一帧做回读,确认这一百帧确实画了东西。
    let img = r.read_target(&target).expect("readback");
    assert!(lit_pixels(&img) > 0);
}

/// A3:同网格同材质归成一次实例化绘制;换一种材质多一次。
#[test]
#[ignore = "requires a local GPU adapter (none in CI)"]
fn instancing_batches_by_mesh_and_material() {
    let r = renderer();
    let cam = camera();
    let mesh = upload(&r, &cube(1.0));
    let red = material(&r, [1.0, 0.0, 0.0, 1.0]);
    let blue = material(&r, [0.0, 0.0, 1.0, 1.0]);
    let target = r.create_target(W, H).expect("target");

    let mut list = DrawList::new();
    for i in 0..64 {
        list.push(&mesh, &red, translate(i as f32 - 32.0, 0.0, 0.0));
    }
    let stats = frame(&r, &list, &cam, &target);
    assert_eq!(stats.draw_calls, 1);
    assert_eq!(stats.instances, 64);

    // 交替材质:调用方给的顺序被打乱,但段数只由(网格,材质)组合数决定。
    for i in 0..64 {
        list.push(
            &mesh,
            if i % 2 == 0 { &red } else { &blue },
            translate(0.0, 5.0, 0.0),
        );
    }
    let stats = frame(&r, &list, &cam, &target);
    assert_eq!(stats.draw_calls, 2);
    assert_eq!(stats.instances, 128);

    let mesh2 = upload(&r, &box_mesh(2.0, 0.5, 0.5));
    list.push(&mesh2, &red, translate(0.0, -5.0, 0.0));
    let stats = frame(&r, &list, &cam, &target);
    assert_eq!(stats.draw_calls, 3);
}

fn render_one(
    r: &HeadlessRenderer,
    mesh: &GpuMesh,
    mat: &GpuMaterial,
    model: [[f32; 4]; 4],
) -> RenderedImage {
    let cam = camera();
    let target = r.create_target(W, H).expect("target");
    let mut list = DrawList::new();
    list.push(mesh, mat, model);
    frame(r, &list, &cam, &target);
    r.read_target(&target).expect("readback")
}

/// 模型矩阵确实生效:平移移动质心、缩放改变投影面积、旋转改变朝向。
/// 平移一项同时钉死矩阵的行列约定——传错成按行上传,平移会落到 w 分量上,
/// 物体直接消失。
#[test]
#[ignore = "requires a local GPU adapter (none in CI)"]
fn model_matrix_translates_scales_and_rotates() {
    let r = renderer();
    let mesh = upload(&r, &cube(5.0));
    let mat = material(&r, [0.7, 0.7, 0.7, 1.0]);

    let base = render_one(&r, &mesh, &mat, identity());
    let (bx, _) = centroid(&base);
    assert!((bx - f64::from(W) / 2.0).abs() < 2.0, "centre x {bx}");

    let right = render_one(&r, &mesh, &mat, translate(20.0, 0.0, 0.0));
    let (rx, ry) = centroid(&right);
    assert!(rx > bx + 10.0, "translate +x must move right: {bx} -> {rx}");
    assert!((ry - f64::from(H) / 2.0).abs() < 2.0, "centre y {ry}");

    let big = render_one(&r, &mesh, &mat, scale(3.0));
    let (small, large) = (lit_pixels(&base), lit_pixels(&big));
    eprintln!("scale: {small} -> {large} lit pixels");
    assert!(
        large > small * 4,
        "3x scale should grow area ~9x: {small} -> {large}"
    );

    // 沿 X 的长条绕 Y 转 90 度后指向镜头,投影明显变短。
    let bar = upload(&r, &box_mesh(30.0, 3.0, 3.0));
    let along_x = render_one(&r, &bar, &mat, identity());
    let along_z = render_one(&r, &bar, &mat, rotate_y(90.0));
    let (a, b) = (lit_pixels(&along_x), lit_pixels(&along_z));
    eprintln!("rotate: {a} -> {b} lit pixels");
    assert!(
        a > b * 4,
        "rotating a long bar toward the camera: {a} -> {b}"
    );
}

/// `pixel()` 返回 BGRA:红在眼前时 R(下标 2)明显大于 B(下标 0),蓝在前时相反。
/// 正对镜头的面不受直射光,只有环境项,亮度不高,所以只比 R 与 B 的相对大小。
fn is_red(p: [u8; 4]) -> bool {
    p[2] > p[0] + 20
}

fn is_blue(p: [u8; 4]) -> bool {
    p[0] > p[2] + 20
}

/// 深度测试:近处遮挡远处。这里材质 id 的顺序与深度顺序相反,排序后近处的
/// 那个反而后画——它不能被先画的远处那个盖住。
#[test]
#[ignore = "requires a local GPU adapter (none in CI)"]
fn near_instance_is_drawn_over_far_one() {
    let r = renderer();
    let cam = camera();
    let mesh = upload(&r, &cube(10.0));
    let red = material(&r, [1.0, 0.0, 0.0, 1.0]);
    let blue = material(&r, [0.0, 0.0, 1.0, 1.0]);
    let target = r.create_target(W, H).expect("target");

    let mut list = DrawList::new();
    list.push(&mesh, &blue, translate(0.0, 0.0, 10.0));
    list.push(&mesh, &red, translate(0.0, 0.0, -10.0));
    frame(&r, &list, &cam, &target);
    let p = r
        .read_target(&target)
        .expect("readback")
        .pixel(W / 2, H / 2);
    eprintln!("near blue, far red: centre pixel {p:?}");
    assert!(is_blue(p), "centre must be the near blue cube, got {p:?}");
}

/// 同上,材质 id 的顺序与深度顺序一致:近处的先画,远处的后画,
/// 后画的远处那个不能盖住它。
#[test]
#[ignore = "requires a local GPU adapter (none in CI)"]
fn far_instance_drawn_later_does_not_cover_near_one() {
    let r = renderer();
    let cam = camera();
    let mesh = upload(&r, &cube(10.0));
    let red = material(&r, [1.0, 0.0, 0.0, 1.0]);
    let blue = material(&r, [0.0, 0.0, 1.0, 1.0]);
    let target = r.create_target(W, H).expect("target");
    let mut list = DrawList::new();
    list.push(&mesh, &red, translate(0.0, 0.0, 10.0));
    list.push(&mesh, &blue, translate(0.0, 0.0, -10.0));
    frame(&r, &list, &cam, &target);
    let p = r
        .read_target(&target)
        .expect("readback")
        .pixel(W / 2, H / 2);
    eprintln!("near red, far blue: centre pixel {p:?}");
    assert!(is_red(p), "centre must stay red, got {p:?}");
}

/// 实例数超出容量时报错而不是悄悄重建;显式扩容后恢复。
#[test]
#[ignore = "requires a local GPU adapter (none in CI)"]
fn over_capacity_errors_until_reserved() {
    let r = renderer();
    let cam = camera();
    let mesh = upload(&r, &cube(0.5));
    let mat = material(&r, [0.5, 0.8, 0.5, 1.0]);
    let target = r.create_target(W, H).expect("target");
    assert_eq!(r.instance_capacity(), DEFAULT_INSTANCE_CAPACITY);

    let over = usize::try_from(DEFAULT_INSTANCE_CAPACITY).unwrap() + 1;
    let mut list = DrawList::new();
    for _ in 0..over {
        list.push(&mesh, &mat, identity());
    }
    let err = r
        .render_draw_list(&list, &cam, &target)
        .unwrap_err()
        .to_string();
    assert!(err.contains("reserve_instances"), "{err}");

    r.reserve_instances(over as u32);
    assert_eq!(r.instance_capacity(), over as u32);

    // `created_*` 是"相对上一帧"的增量:此前没有成功渲染过帧,所以第一帧
    // 如实报出至今为止的全部新建——网格 2 个、材质 1 个、扩容 1 个。
    let first = frame(&r, &list, &cam, &target);
    assert_eq!(first.instances as usize, over);
    assert_eq!(first.created_buffers, 4);
    // 之后每帧都应是 0:扩容只被报一次。
    for _ in 0..10 {
        let s = frame(&r, &list, &cam, &target);
        assert_eq!(
            (s.created_buffers, s.created_bind_groups, s.created_textures),
            (0, 0, 0)
        );
    }

    // 只增不减:申请更小的容量不生效。
    r.reserve_instances(8);
    assert_eq!(r.instance_capacity(), over as u32);
}

/// 输入不合法时在 CPU 侧显式拒绝,不赌 GPU 行为。
#[test]
#[ignore = "requires a local GPU adapter (none in CI)"]
fn invalid_input_is_rejected() {
    let r = renderer();
    let (positions, normals, indices) = cube(1.0);
    assert!(r.upload_mesh(&[], Some(&normals), None, &indices).is_err());
    assert!(r
        .upload_mesh(&positions, Some(&normals), None, &[])
        .is_err());
    assert!(r
        .upload_mesh(&positions, Some(&normals[..3]), None, &indices)
        .is_err());
    assert!(r
        .upload_mesh(&positions, None, Some(&[[0.0, 0.0]; 2]), &indices)
        .is_err());
    let mut bad = indices.clone();
    bad[0] = positions.len() as u32;
    assert!(r
        .upload_mesh(&positions, Some(&normals), None, &bad)
        .is_err());

    assert!(r.create_target(0, H).is_err());
    assert!(r.create_target(W, 0).is_err());
}

/// 相机尺寸与渲染目标不一致时报错,而不是画一张被拉伸的图。
#[test]
#[ignore = "requires a local GPU adapter (none in CI)"]
fn camera_must_match_target() {
    let r = renderer();
    let mesh = upload(&r, &cube(1.0));
    let mat = material(&r, [0.5, 0.5, 0.5, 1.0]);
    let target = r.create_target(W, H).expect("target");
    let mut list = DrawList::new();
    list.push(&mesh, &mat, identity());
    let other = Camera::look_at(
        [0.0, 0.0, 90.0],
        [0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        60.0,
        W * 2,
        H,
        0.1,
        500.0,
    );
    let err = r
        .render_draw_list(&list, &other, &target)
        .unwrap_err()
        .to_string();
    assert!(err.contains("camera size"), "{err}");
    // 空列表同样要过这一道检查。
    assert!(r
        .render_draw_list(&DrawList::new(), &other, &target)
        .is_err());
}

/// 空列表是合法的一帧:只清屏。先画一个方块,再交一帧空的,画面应回到背景色。
#[test]
#[ignore = "requires a local GPU adapter (none in CI)"]
fn empty_draw_list_clears_the_target() {
    let r = renderer();
    let cam = camera();
    let mesh = upload(&r, &cube(8.0));
    let mat = material(&r, [0.9, 0.9, 0.9, 1.0]);
    let target = r.create_target(W, H).expect("target");
    let mut list = DrawList::new();
    list.push(&mesh, &mat, identity());
    frame(&r, &list, &cam, &target);
    assert!(lit_pixels(&r.read_target(&target).unwrap()) > 0);

    list.clear();
    let stats = frame(&r, &list, &cam, &target);
    assert_eq!((stats.draw_calls, stats.instances), (0, 0));
    let img = r.read_target(&target).expect("readback");
    assert_eq!(
        lit_pixels(&img),
        0,
        "an empty frame must leave a cleared target"
    );
}

/// 改尺寸是显式动作:尺寸没变不重建,变了才重建两张纹理。
#[test]
#[ignore = "requires a local GPU adapter (none in CI)"]
fn resize_rebuilds_only_when_size_changes() {
    let r = renderer();
    let cam = camera();
    let mesh = upload(&r, &cube(8.0));
    let mat = material(&r, [0.9, 0.9, 0.9, 1.0]);
    let mut target = r.create_target(W, H).expect("target");
    let mut list = DrawList::new();
    list.push(&mesh, &mat, identity());
    frame(&r, &list, &cam, &target);

    r.resize_target(&mut target, W, H).expect("same size");
    assert_eq!(frame(&r, &list, &cam, &target).created_textures, 0);

    let (sw, sh) = (W / 2, H / 2);
    r.resize_target(&mut target, sw, sh).expect("smaller");
    assert_eq!((target.width(), target.height()), (sw, sh));
    // 尺寸变了之后,旧尺寸的相机必须被拒绝。
    assert!(r.render_draw_list(&list, &cam, &target).is_err());
    let small = Camera::look_at(
        [0.0, 0.0, 90.0],
        [0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        60.0,
        sw,
        sh,
        0.1,
        500.0,
    );
    let stats = frame(&r, &list, &small, &target);
    assert_eq!(stats.created_textures, 2, "colour + depth");
    let img = r.read_target(&target).expect("readback");
    assert_eq!((img.width, img.height), (sw, sh));
    assert!(lit_pixels(&img) > 0);
    assert!(r.resize_target(&mut target, 0, sh).is_err());
}

// ─── 步 1b:异步回读 ───────────────────────────────────────────────────────────
//
// B1 逐帧与同步回读像素一致(含多帧在途,验证缓冲复用不会串帧)
// B2 `poll_read` 不阻塞地问、重复取或作废的回执报错而不是给错图
// B3 回读缓冲进池复用:稳态零新建,池子按在途深度增长
// B4 在途超过上限时背压;没人取的旧结果按序作废,取到的仍是各自那一帧
// B5 回执记着自己提交时的尺寸,目标改尺寸不影响在途的那张

/// 第 k 帧:把方块摆在随 k 变化的位置上,保证相邻帧画面不同。
fn moving_frame(
    r: &HeadlessRenderer,
    mesh: &GpuMesh,
    mat: &GpuMaterial,
    cam: &Camera,
    target: &RenderTarget,
    k: usize,
) -> FrameStats {
    let mut list = DrawList::new();
    list.push(mesh, mat, translate(-24.0 + 8.0 * k as f32, 0.0, 0.0));
    frame(r, &list, cam, target)
}

fn camera_at(w: u32, h: u32) -> Camera {
    Camera::look_at(
        [0.0, 0.0, 90.0],
        [0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        60.0,
        w,
        h,
        0.1,
        500.0,
    )
}

/// B1:同一串帧,异步取回的像素必须与"渲染后立刻同步回读"逐字节相同。
/// 两种取法都测:提交后立刻等(深度 1),以及四帧都提交了再逐张取(深度 4,
/// 走的是不同的回读缓冲)。参照帧之间必须互不相同,否则这个断言是空的。
#[test]
#[ignore = "requires a local GPU adapter (none in CI)"]
fn async_readback_matches_sync_frame_by_frame() {
    let r = renderer();
    let cam = camera();
    let target = r.create_target(W, H).expect("target");
    let mesh = upload(&r, &cube(6.0));
    let mat = material(&r, [0.8, 0.3, 0.2, 1.0]);
    let frames = 12;

    let reference: Vec<RenderedImage> = (0..frames)
        .map(|k| {
            moving_frame(&r, &mesh, &mat, &cam, &target, k);
            r.read_target(&target).expect("sync readback")
        })
        .collect();
    for pair in reference.windows(2) {
        assert_ne!(pair[0].bgra, pair[1].bgra, "reference frames must differ");
    }

    // 深度 1:提交后立刻等。
    for (k, want) in reference.iter().enumerate() {
        moving_frame(&r, &mesh, &mat, &cam, &target, k);
        let ticket = r.read_target_async(&target).expect("async readback");
        let got = r.wait_read(ticket).expect("wait_read");
        assert_eq!(got.bgra, want.bgra, "depth-1 frame {k}");
    }
    assert_eq!(r.reads_in_flight(), 0);

    // 深度 4:一批四帧全部提交后再取。
    for (batch, want) in reference.chunks(MAX_READS_IN_FLIGHT).enumerate() {
        let mut tickets = Vec::new();
        for (j, _) in want.iter().enumerate() {
            let k = batch * MAX_READS_IN_FLIGHT + j;
            moving_frame(&r, &mesh, &mat, &cam, &target, k);
            tickets.push(r.read_target_async(&target).expect("async readback"));
        }
        for (j, ticket) in tickets.iter().enumerate() {
            let got = r.wait_read(*ticket).expect("wait_read");
            assert_eq!(got.bgra, want[j].bgra, "depth-4 batch {batch} frame {j}");
        }
    }
}

/// B2:`poll_read` 没好时返回 `Ok(None)`,好了给出与同步回读相同的像素;
/// 一张回执只能取一次,再取报错——而不是把缓冲里后来的内容当成它。
#[test]
#[ignore = "requires a local GPU adapter (none in CI)"]
fn poll_read_reports_pending_then_delivers_once() {
    let r = renderer();
    let cam = camera();
    let target = r.create_target(W, H).expect("target");
    let mesh = upload(&r, &cube(6.0));
    let mat = material(&r, [0.8, 0.3, 0.2, 1.0]);

    moving_frame(&r, &mesh, &mat, &cam, &target, 0);
    let want = r.read_target(&target).expect("sync readback");
    moving_frame(&r, &mesh, &mat, &cam, &target, 0);
    let ticket = r.read_target_async(&target).expect("async readback");

    let mut polls = 0usize;
    let got = loop {
        match r.poll_read(ticket).expect("ticket is in flight") {
            Some(img) => break img,
            None => {
                polls += 1;
                // 软件光栅化下一帧是毫秒级;真卡更快。上限只是防止测试挂死。
                assert!(polls < 2_000_000, "poll_read never became ready");
            }
        }
    };
    assert_eq!(got.bgra, want.bgra);
    assert!(got.width == W && got.height == H);
    assert!(
        r.poll_read(ticket).is_err(),
        "a ticket must not deliver twice"
    );
    assert!(r.wait_read(ticket).is_err());
}

/// B3:回读缓冲是持久资源。一张一张取只需要 1 块缓冲;四帧在途时池子长到
/// 4 块;之后无论取多少次都不再新建——由 `FrameStats::created_buffers` 自证。
#[test]
#[ignore = "requires a local GPU adapter (none in CI)"]
fn async_readback_reuses_buffers_in_steady_state() {
    let r = renderer();
    let cam = camera();
    let target = r.create_target(W, H).expect("target");
    let mesh = upload(&r, &cube(6.0));
    let mat = material(&r, [0.8, 0.3, 0.2, 1.0]);
    // 创建发生在 `read_target_async` 里,所以会记在**下一帧**的统计上;
    // 每轮末尾多渲一帧把最后一次创建也收进来。开始前先渲一帧:上传网格与
    // 建材质那 3 块缓冲不是本测试要数的东西,不能混进第一帧。
    let mut k = 0usize;
    let tick = |r: &HeadlessRenderer, k: usize| -> FrameStats {
        moving_frame(r, &mesh, &mat, &cam, &target, k)
    };
    tick(&r, k);
    let mut created = 0u32;

    for _ in 0..8 {
        created += tick(&r, k).created_buffers;
        k += 1;
        let t = r.read_target_async(&target).expect("async readback");
        r.wait_read(t).expect("wait_read");
    }
    created += tick(&r, k).created_buffers;
    assert_eq!(
        created, 1,
        "collecting one at a time needs exactly one readback buffer"
    );

    created = 0;
    for _ in 0..6 {
        let mut tickets = Vec::new();
        for _ in 0..MAX_READS_IN_FLIGHT {
            created += tick(&r, k).created_buffers;
            k += 1;
            tickets.push(r.read_target_async(&target).expect("async readback"));
        }
        for t in tickets {
            r.wait_read(t).expect("wait_read");
        }
    }
    created += tick(&r, k).created_buffers;
    assert_eq!(
        created,
        (MAX_READS_IN_FLIGHT - 1) as u32,
        "pool grows from one buffer to the in-flight depth"
    );
    assert_eq!(r.reads_in_flight(), 0);
}

/// B4:不取结果、连续发起远超上限的回读。在途张数始终不超上限(满了就等);
/// 取的时候,作废的一定是最旧的那些(前缀),没作废的像素仍与各自那一帧一致。
#[test]
#[ignore = "requires a local GPU adapter (none in CI)"]
fn async_readback_applies_backpressure_and_drops_oldest() {
    let r = renderer();
    let cam = camera();
    let target = r.create_target(W, H).expect("target");
    let mesh = upload(&r, &cube(6.0));
    let mat = material(&r, [0.8, 0.3, 0.2, 1.0]);
    let total = 3 * MAX_READS_IN_FLIGHT;

    let reference: Vec<RenderedImage> = (0..total)
        .map(|k| {
            moving_frame(&r, &mesh, &mat, &cam, &target, k);
            r.read_target(&target).expect("sync readback")
        })
        .collect();

    let mut tickets = Vec::new();
    for k in 0..total {
        moving_frame(&r, &mesh, &mat, &cam, &target, k);
        tickets.push(r.read_target_async(&target).expect("async readback"));
        assert!(
            r.reads_in_flight() <= MAX_READS_IN_FLIGHT,
            "more reads in flight than allowed at request {k}"
        );
    }

    let mut seen_kept = false;
    let (mut kept, mut dropped) = (0usize, 0usize);
    for (k, ticket) in tickets.iter().enumerate() {
        match r.wait_read(*ticket) {
            Ok(img) => {
                seen_kept = true;
                assert_eq!(img.bgra, reference[k].bgra, "frame {k}");
                kept += 1;
            }
            Err(_) => {
                // 作废的只能是最旧的那一段:出现过保留的之后,不许再作废。
                assert!(
                    !seen_kept,
                    "ticket {k} was dropped although an older one was kept"
                );
                dropped += 1;
            }
        }
    }
    // 在途加上待取最多各留一组,所以 12 张里至少作废 4 张、至少留下 1 张。
    assert!(
        dropped >= total - 2 * MAX_READS_IN_FLIGHT,
        "dropped {dropped}"
    );
    assert!((1..=2 * MAX_READS_IN_FLIGHT).contains(&kept), "kept {kept}");
    assert_eq!(r.reads_in_flight(), 0);
}

/// B5:回执记的是提交那一刻的尺寸。之后目标改了尺寸,在途的那张读出来仍是
/// 旧尺寸、旧画面;新尺寸的回读正常。
#[test]
#[ignore = "requires a local GPU adapter (none in CI)"]
fn async_readback_keeps_its_size_across_resize() {
    let r = renderer();
    let cam = camera();
    let mut target = r.create_target(W, H).expect("target");
    let mesh = upload(&r, &cube(6.0));
    let mat = material(&r, [0.8, 0.3, 0.2, 1.0]);

    moving_frame(&r, &mesh, &mat, &cam, &target, 0);
    let want = r.read_target(&target).expect("sync readback");
    moving_frame(&r, &mesh, &mat, &cam, &target, 0);
    let ticket = r.read_target_async(&target).expect("async readback");

    let (w2, h2) = (W + 96, H + 48);
    r.resize_target(&mut target, w2, h2).expect("resize");
    let got = r.wait_read(ticket).expect("wait_read");
    assert_eq!((got.width, got.height), (W, H));
    assert_eq!(got.bgra, want.bgra);

    let cam2 = camera_at(w2, h2);
    moving_frame(&r, &mesh, &mat, &cam2, &target, 1);
    let got = r
        .wait_read(r.read_target_async(&target).expect("async readback"))
        .expect("wait_read");
    assert_eq!((got.width, got.height), (w2, h2));
    assert!(lit_pixels(&got) > 0);
}
