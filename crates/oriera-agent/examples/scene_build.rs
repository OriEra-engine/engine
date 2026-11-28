//! 真实场景搭建端到端(scene batch 收官证据链):一句话的结构化形态
//! [`SceneIntent`] → L9 编排布局([`oriera_agent::scene_planner::plan`]) →
//! L4 地形生成与地表散布贴地 → L5 多 part PBR 无头渲染 → BMP 落盘。
//!
//! ```text
//! cargo run -p oriera-agent --release --example scene_build -- \
//!   --out scene.bmp [--size WxH] [--seed N] [--trees N] [--rocks N]
//! ```
//!
//! 链路上每个环节都来自真实场景批次已合并的模块:地形/散布(WP5)、
//! 编排(WP10)、剔除与合批统计(WP3/WP4 词汇,本例打印统计)、渲染
//! (RFC 0004 之前的快照路径 `render_scene_views`)。天空/IBL(WP2/WP6)
//! 与 GPU 持久路径(#207)的接线是后续批次——本例背景是引擎固定清屏色。
//!
//! 确定性:CPU 全管线(地形/散布/布局/烘焙矩阵)双跑逐位一致,GPU 同设备
//! 双渲染逐像素一致——任一失败即退出码 1。
//!
//! 双显卡机器注意(RFC 0004 开放问题 4,本例实测复现):默认适配器会选到
//! 集成显卡,老核显驱动走 Vulkan 在 wgpu-hal 初始化 segfault——请用
//! `WGPU_BACKEND=dx12` 前缀运行(同一台机器 DX12 下 Intel UHD 正常)。
//!
//! 坐标约定陷阱(#200 教训的镜像面):`Transform::to_matrix` 返回 glam
//! 列主序矩阵,而 `camera::Mat4` 行主序——同一线性映射的两种存储,转换
//! =转置。启动时自检(见 [`self_check_conventions`]),错约定当场 panic。

use oriera_agent::scene_planner::{plan, PlacementIntent, PlannerError, SceneIntent};
use oriera_asset::scatter::{scatter, ScatterSpec};
use oriera_asset::terrain::{Terrain, TerrainSpec};
use oriera_common::math::{Mat4 as GlamMat4, Quat};
use oriera_common::{Transform, Vec3};
use oriera_render::camera::{self, Camera};
use oriera_render::cull::{cull, CullParams, Frustum};
use oriera_render::gpu::{HeadlessRenderer, RenderedImage, SceneEnv, ScenePart};
use oriera_render::instancing::batch_by_key;
use oriera_render::scene::{Bounds, SceneInstance};

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let out = get("--out").unwrap_or_else(|| "scene_build.bmp".into());
    let (width, height) = match get("--size") {
        Some(s) => {
            let (w, h) = s.split_once('x').ok_or("--size expects WxH")?;
            (
                w.parse::<u32>().map_err(|e| format!("{e}"))?,
                h.parse::<u32>().map_err(|e| format!("{e}"))?,
            )
        }
        None => (1280, 720),
    };
    let seed: u64 = match get("--seed") {
        Some(s) => s.parse().map_err(|e| format!("{e}"))?,
        None => 20261128,
    };
    let trees: usize = match get("--trees") {
        Some(s) => s.parse().map_err(|e| format!("{e}"))?,
        None => 150,
    };
    let rocks: u32 = match get("--rocks") {
        Some(s) => s.parse().map_err(|e| format!("{e}"))?,
        None => 14,
    };

    self_check_conventions();

    // ── 1. 地形(WP5):种子化高度场,160m×160m ─────────────────────────
    let t0 = std::time::Instant::now();
    let spec = TerrainSpec::new(160, 160, 1.0, seed, 5, 7.0);
    let terrain = Terrain::generate(spec);
    let terrain_mesh = terrain.build_mesh(1);
    let terrain_normals: Vec<[f32; 3]> = terrain_mesh
        .positions
        .iter()
        .map(|p| terrain.normal_at(p[0], p[2]))
        .collect();
    println!(
        "stage1 terrain | {} verts {} tris | gen {:.1} ms",
        terrain_mesh.positions.len(),
        terrain_mesh.indices.len() / 3,
        t0.elapsed().as_secs_f64() * 1e3
    );

    // ── 2. 植被散布(WP5):贴地 + 坡度剔除 + LCG 尺度/朝向 ───────────────
    // density 目标 = trees / 面积;floor 边界规则会少收,截到恰好 trees。
    let t1 = std::time::Instant::now();
    let area = 159.0 * 159.0;
    let density = trees as f32 * 100.0 / area;
    let sspec = ScatterSpec::new(density, seed ^ 0x5EED, 0.8, 1.5, 32.0);
    let mut pines = scatter(&terrain, &sspec);
    pines.truncate(trees);
    println!(
        "stage2 scatter | {} pines placed (slope≤32°) | {:.1} ms",
        pines.len(),
        t1.elapsed().as_secs_f64() * 1e3
    );

    // ── 3. 营地编排(WP10):结构化意图 → 确定性布局 ────────────────────
    // 场地中心在原点([-area_half,area_half]²),平移到地形中心后再贴地。
    let t2 = std::time::Instant::now();
    let center = [80.0f32, 80.0f32];
    let intent = SceneIntent {
        area_half: 10.0,
        seed: seed ^ 0xCAFE,
        placements: vec![
            PlacementIntent {
                asset_name: "rock".into(),
                count: rocks,
                min_separation: 3.0,
                anchor: None,
            },
            PlacementIntent {
                asset_name: "campfire".into(),
                count: 1,
                min_separation: 0.0,
                anchor: Some(([0.0, 0.0, 0.0], 0.0)),
            },
            PlacementIntent {
                asset_name: "tent".into(),
                count: 1,
                min_separation: 0.0,
                anchor: Some(([5.5, 0.0, 2.5], 35.0)),
            },
        ],
    };
    let available = vec![
        "rock".to_string(),
        "campfire".to_string(),
        "tent".to_string(),
    ];
    let layout = plan(&intent, &available).map_err(|e: PlannerError| e.to_string())?;
    println!(
        "stage3 planner | {} items laid out (intent→layout) | {:.1} ms",
        layout.items.len(),
        t2.elapsed().as_secs_f64() * 1e3
    );

    // ── 4. 烘焙:程序化网格 × 摆放矩阵 → 世界空间 part ──────────────────
    // 场地意图坐标 → 地形系:平移 center;全部贴地 y=sample_bilinear。
    let t3 = std::time::Instant::now();
    let campsite: Vec<(String, Transform)> = layout
        .items
        .iter()
        .map(|item| {
            let mut tf = item.transform;
            let mut p = tf.translation;
            p.x += center[0];
            p.z += center[1];
            p.y = terrain.sample_bilinear(p.x, p.z);
            tf.translation = p;
            (item.asset_name.clone(), tf)
        })
        .collect();

    // 松树模板(局部空间,基部 y=0):树干与树冠不同材质 → 分两个 part。
    let pine_trunk = trunk_mesh();
    let pine_foliage = foliage_mesh();
    let rock_tpl = rock_mesh(seed ^ 0x70C5);
    let campfire_tpl = campfire_mesh();
    let tent_tpl = tent_mesh();

    // 聚合缓冲:世界空间顶点 + 面法线(兰伯特烘焙吃法线)。
    let terrain_buf = MeshBuf {
        pos: terrain_mesh.positions.clone(),
        nrm: terrain_normals,
        idx: terrain_mesh.indices,
    };
    let mut trunks = MeshBuf::default();
    let mut foliage = MeshBuf::default();
    let mut rocks = MeshBuf::default();
    let mut fires = MeshBuf::default();
    let mut tents = MeshBuf::default();

    // 松树:散布已给贴地 Transform(含抖动 yaw 与均匀 scale)。
    for tf in &pines {
        let m = transform_to_camera(tf);
        trunks.append_transformed(&pine_trunk, &m);
        foliage.append_transformed(&pine_foliage, &m);
    }
    // 营地:编排布局(锚点精确落位/散布 min_sep)→ LCG 尺度变化(仅岩石)。
    let mut lcg = Lcg(seed ^ 0xBADC0DE);
    for (name, tf) in &campsite {
        let mut tf = *tf;
        if name == "rock" {
            let s = 0.5 + lcg.next_f32() * 0.7;
            tf.scale = Vec3::splat(s);
        }
        let m = transform_to_camera(&tf);
        match name.as_str() {
            "rock" => rocks.append_transformed(&rock_tpl, &m),
            "campfire" => fires.append_transformed(&campfire_tpl, &m),
            "tent" => tents.append_transformed(&tent_tpl, &m),
            other => return Err(format!("未知资产 {other}")),
        }
    }
    println!(
        "stage4 bake | parts=6 | {} pine/{} rock baked | {:.1} ms",
        pines.len(),
        campsite.iter().filter(|(n, _)| n == "rock").count(),
        t3.elapsed().as_secs_f64() * 1e3
    );
    let parts_buf = Parts {
        terrain: terrain_buf,
        trunks,
        foliage,
        rocks,
        fires,
        tents,
    };

    // ── 5. 剔除与合批统计(WP3/WP4 真实接入):统计口径,渲染仍全量 ───────
    let cam = Camera::look_at(
        // 相机放受光面(SCENE_LIGHT_DIR [-0.45,0.55,-0.70] 的来光侧),否则
        // 朝向镜头的法线与光向点积为负、兰伯特全黑(#152 shader 语义)。
        [
            center[0] - 42.0,
            terrain.sample_bilinear(center[0] - 42.0, center[1] - 38.0) + 14.0,
            center[1] - 38.0,
        ],
        [center[0], 1.5, center[1]],
        [0.0, 1.0, 0.0],
        50.0,
        width,
        height,
        0.1,
        500.0,
    );
    let mut insts: Vec<SceneInstance> = Vec::new();
    for tf in &pines {
        let m = transform_to_camera(tf);
        insts.push(SceneInstance::new(
            0,
            0,
            m,
            Bounds::new([0.0, 1.6, 0.0], 1.9),
        ));
    }
    for (name, tf) in &campsite {
        let mesh_id = match name.as_str() {
            "rock" => 1,
            "campfire" => 2,
            _ => 3,
        };
        let r = if name == "rock" {
            tf.scale.x * 0.9
        } else {
            1.2
        };
        insts.push(SceneInstance::new(
            mesh_id,
            mesh_id,
            transform_to_camera(tf),
            Bounds::new([0.0, 0.6, 0.0], r),
        ));
    }
    let frustum = Frustum::from_view_proj(&cam.view_proj());
    let cull = cull(
        &insts,
        &frustum,
        &CullParams {
            camera_pos: cam.eye_pos(),
            max_distance: 320.0,
            lod_thresholds: [0.08, 0.035],
        },
    );
    let batches = batch_by_key(&insts);
    println!(
        "stage5 stats | instances {} → visible {} (frustum −{}, distance −{}) | batches {}",
        cull.stats.tested,
        cull.visible.len(),
        cull.stats.frustum_rejected,
        cull.stats.distance_rejected,
        batches.len()
    );

    // ── 6. 渲染(同设备双跑逐像素)+ BMP 落盘 ───────────────────────────
    // 白天化接入:光向=太阳方向(兰伯特直接受益);清屏=显示域美术天空色。
    // 诚实边界:SceneEnv.clear 是显示域直写——WP6 的 HDR 天空辐射→背景面
    // (天空盒/背景 pass)与 WP2 环境项进 shader 属 #207 后续批次,此处不
    // 过 tonemap 链硬凑(近白 tint 过 ACES 只会更白)。
    let env = SceneEnv {
        clear: [0.55, 0.72, 0.98, 1.0],
        light_dir: norm3([-0.55, 0.5, -0.4]),
        light_intensity: 1.7,
    };
    println!(
        "stage6 env | clear=({:.2},{:.2},{:.2}) sun=({:.2},{:.2},{:.2}) I={:.1}",
        env.clear[0],
        env.clear[1],
        env.clear[2],
        env.light_dir[0],
        env.light_dir[1],
        env.light_dir[2],
        env.light_intensity
    );
    let renderer = HeadlessRenderer::new().map_err(|e| e.to_string())?;
    println!("adapter: {}", renderer.adapter_info());
    let parts = mk_parts(&parts_buf);
    let t4 = std::time::Instant::now();
    let img_a = renderer
        .render_scene_views_env(&parts, &cam, &env)
        .map_err(|e| e.to_string())?;
    let img_b = renderer
        .render_scene_views_env(&parts, &cam, &env)
        .map_err(|e| e.to_string())?;
    let pixels = pixels_of(&img_a);
    let pixels_b = pixels_of(&img_b);
    if pixels != pixels_b {
        return Err("GPU 双渲染逐像素不一致——确定性红线".into());
    }
    println!(
        "stage6 render | {width}x{height} | {:.1} ms | 双渲染逐像素一致 ✓",
        t4.elapsed().as_secs_f64() * 1e3
    );
    drop(parts);

    // ── 7. CPU 管线确定性双跑(第二遍全链,与首遍逐位比对)────────────────
    let t5 = std::time::Instant::now();
    let spec2 = TerrainSpec::new(160, 160, 1.0, seed, 5, 7.0);
    let terrain2 = Terrain::generate(spec2);
    let mesh2 = terrain2.build_mesh(1);
    let sspec2 = ScatterSpec::new(density, seed ^ 0x5EED, 0.8, 1.5, 32.0);
    let pines2 = scatter(&terrain2, &sspec2);
    let intent2 = SceneIntent {
        area_half: 10.0,
        seed: seed ^ 0xCAFE,
        placements: intent.placements.clone(),
    };
    let layout2 = plan(&intent2, &available).map_err(|e: PlannerError| e.to_string())?;
    let same = mesh2.positions == terrain_mesh.positions
        && pines2.len() == pines.len()
        && pines2.iter().zip(&pines).all(|(a, b)| {
            a.translation == b.translation && a.rotation == b.rotation && a.scale == b.scale
        })
        && layout2.items.len() == layout.items.len()
        && layout2
            .items
            .iter()
            .zip(&layout.items)
            .all(|(a, b)| a.asset_name == b.asset_name && a.transform == b.transform);
    if !same {
        return Err("CPU 管线双跑不一致——确定性红线".into());
    }
    println!(
        "stage7 determinism | CPU 全链双跑逐位一致 ✓ | {:.1} ms",
        t5.elapsed().as_secs_f64() * 1e3
    );

    write_bmp(&out, &img_a).map_err(|e| format!("{out}: {e}"))?;
    println!("wrote {out}");
    Ok(())
}

// ── 约定自检(#200 教训的镜像面)─────────────────────────────────────────
/// glam 列主序 ↔ camera 行主序的转置转换,与两族矩阵的点变换必须在启动时
/// 对拍一致——错约定渲染出的场景会整体错位/镜像,当场 panic 比 PNG 里找茬强。
fn self_check_conventions() {
    let t = Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2));
    let m = transform_to_camera(&t);
    let p = camera::transform(&m, [1.0, 0.0, 0.0]);
    assert!(
        ((p[0] - 0.0).abs() < 1e-5)
            && ((p[1] - 0.0).abs() < 1e-5)
            && ((p[2] - (-1.0)).abs() < 1e-5),
        "glam↔camera 矩阵约定自检失败:rotY(+90°) 应把 +x 映到 -z,得到 {p:?}"
    );
    let t2 = Transform::from_translation(Vec3::new(3.0, -1.0, 2.0));
    let m2 = transform_to_camera(&t2);
    let p2 = camera::transform(&m2, [0.0, 0.0, 0.0]);
    assert!(
        ((p2[0] - 3.0).abs() < 1e-5)
            && ((p2[1] - (-1.0)).abs() < 1e-5)
            && ((p2[2] - 2.0).abs() < 1e-5),
        "平移自检失败: {p2:?}"
    );
}

/// `Transform::to_matrix()` 是 glam 列主序(`m[c][r]`);`camera::Mat4` 行主序
/// (`m[r][c]`)。同一线性映射的两种存储——转换即转置。
fn transform_to_camera(t: &Transform) -> camera::Mat4 {
    let g: GlamMat4 = t.to_matrix();
    let g = g.to_cols_array_2d();
    let mut out = [[0.0f32; 4]; 4];
    for r in 0..4 {
        for c in 0..4 {
            out[r][c] = g[c][r];
        }
    }
    out
}

/// 仓库 LCG 约定(MMIX 常数,与 oriera_infer 同款)——散布已用,岩石尺度
/// 变化沿用同族,禁 DefaultHasher。
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
    fn next_f32(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

// ── 网格生成(程序化,零外部资产)─────────────────────────────────────────
#[derive(Default)]
struct MeshBuf {
    pos: Vec<[f32; 3]>,
    nrm: Vec<[f32; 3]>,
    idx: Vec<u32>,
}

impl MeshBuf {
    fn push_tri(
        &mut self,
        a: ([f32; 3], [f32; 3]),
        b: ([f32; 3], [f32; 3]),
        c: ([f32; 3], [f32; 3]),
    ) {
        let base = self.pos.len() as u32;
        for (p, n) in [a, b, c] {
            self.pos.push(p);
            self.nrm.push(n);
        }
        self.idx.extend([base, base + 1, base + 2]);
    }

    fn append_transformed(&mut self, other: &MeshBuf, m: &camera::Mat4) {
        let base = self.pos.len() as u32;
        for (p, n) in other.pos.iter().zip(&other.nrm) {
            let wp = camera::transform(m, *p);
            // 法线走 3×3(均匀缩放下 normalize 后正确)。
            let mut wn = [0.0f32; 3];
            for i in 0..3 {
                wn[i] = m[i][0] * n[0] + m[i][1] * n[1] + m[i][2] * n[2];
            }
            let len = (wn[0] * wn[0] + wn[1] * wn[1] + wn[2] * wn[2]).sqrt();
            let wn = if len > f32::MIN_POSITIVE {
                [wn[0] / len, wn[1] / len, wn[2] / len]
            } else {
                [0.0, 1.0, 0.0]
            };
            self.pos.push([wp[0], wp[1], wp[2]]);
            self.nrm.push(wn);
        }
        for i in &other.idx {
            self.idx.push(base + i);
        }
    }
}

/// 圆柱(侧面 + 双端盖),基部 y=0。
fn cylinder(radius: f32, height: f32, sides: usize) -> MeshBuf {
    let mut m = MeshBuf::default();
    let ring = |y: f32| -> Vec<[f32; 3]> {
        (0..sides)
            .map(|s| {
                let th = (s as f32) * std::f32::consts::TAU / sides as f32;
                [radius * th.cos(), y, radius * th.sin()]
            })
            .collect()
    };
    let bot = ring(0.0);
    let top = ring(height);
    for s in 0..sides {
        let s2 = (s + 1) % sides;
        let th = ((s as f32) + 0.5) * std::f32::consts::TAU / sides as f32;
        let n = [th.cos(), 0.0, th.sin()];
        m.push_tri((bot[s], n), (bot[s2], n), (top[s2], n));
        m.push_tri((bot[s], n), (top[s2], n), (top[s], n));
    }
    let c_top = [0.0, height, 0.0];
    let c_bot = [0.0, 0.0, 0.0];
    for s in 0..sides {
        let s2 = (s + 1) % sides;
        m.push_tri(
            (c_top, [0.0, 1.0, 0.0]),
            (top[s], [0.0, 1.0, 0.0]),
            (top[s2], [0.0, 1.0, 0.0]),
        );
        m.push_tri(
            (c_bot, [0.0, -1.0, 0.0]),
            (bot[s2], [0.0, -1.0, 0.0]),
            (bot[s], [0.0, -1.0, 0.0]),
        );
    }
    m
}

/// 圆锥(侧面 + 底盖),基部 y=base_y,尖顶在上。
fn cone(radius: f32, height: f32, sides: usize, base_y: f32) -> MeshBuf {
    let mut m = MeshBuf::default();
    let apex = [0.0, base_y + height, 0.0];
    let ring: Vec<[f32; 3]> = (0..sides)
        .map(|s| {
            let th = (s as f32) * std::f32::consts::TAU / sides as f32;
            [radius * th.cos(), base_y, radius * th.sin()]
        })
        .collect();
    let slope = radius / height;
    for s in 0..sides {
        let s2 = (s + 1) % sides;
        let th = ((s as f32) + 0.5) * std::f32::consts::TAU / sides as f32;
        let n = norm3([th.cos(), slope, th.sin()]);
        m.push_tri((ring[s], n), (ring[s2], n), (apex, n));
    }
    let c = [0.0, base_y, 0.0];
    for s in 0..sides {
        let s2 = (s + 1) % sides;
        m.push_tri(
            (c, [0.0, -1.0, 0.0]),
            (ring[s2], [0.0, -1.0, 0.0]),
            (ring[s], [0.0, -1.0, 0.0]),
        );
    }
    m
}

/// 一个四边形面:四个顶点 + 平面法线(box/rock 共用的网格生成词汇)。
type QuadFace = ([f32; 3], [f32; 3], [f32; 3], [f32; 3], [f32; 3]);

/// 轴对齐盒(6 面 × 平面法线)。
fn box_mesh(w: f32, h: f32, d: f32) -> MeshBuf {
    let mut m = MeshBuf::default();
    let (hx, hy, hz) = (w / 2.0, h / 2.0, d / 2.0);
    let faces: [QuadFace; 6] = [
        (
            [-hx, -hy, hz],
            [hx, -hy, hz],
            [hx, hy, hz],
            [-hx, hy, hz],
            [0.0, 0.0, 1.0],
        ),
        (
            [hx, -hy, -hz],
            [-hx, -hy, -hz],
            [-hx, hy, -hz],
            [hx, hy, -hz],
            [0.0, 0.0, -1.0],
        ),
        (
            [hx, -hy, hz],
            [hx, -hy, -hz],
            [hx, hy, -hz],
            [hx, hy, hz],
            [1.0, 0.0, 0.0],
        ),
        (
            [-hx, -hy, -hz],
            [-hx, -hy, hz],
            [-hx, hy, hz],
            [-hx, hy, -hz],
            [-1.0, 0.0, 0.0],
        ),
        (
            [-hx, hy, hz],
            [hx, hy, hz],
            [hx, hy, -hz],
            [-hx, hy, -hz],
            [0.0, 1.0, 0.0],
        ),
        (
            [-hx, -hy, -hz],
            [hx, -hy, -hz],
            [hx, -hy, hz],
            [-hx, -hy, hz],
            [0.0, -1.0, 0.0],
        ),
    ];
    for (a, b, c, d, n) in faces {
        m.push_tri((a, n), (b, n), (c, n));
        m.push_tri((a, n), (c, n), (d, n));
    }
    m
}

/// 岩石:立方体逐面细分后径向确定性位移,逐面平面法线。
fn rock_mesh(seed: u64) -> MeshBuf {
    let mut m = MeshBuf::default();
    let (hx, hy, hz) = (0.8f32, 0.6f32, 0.8f32);
    let faces: [QuadFace; 6] = [
        (
            [-hx, -hy, hz],
            [hx, -hy, hz],
            [hx, hy, hz],
            [-hx, hy, hz],
            [0.0, 0.0, 1.0],
        ),
        (
            [hx, -hy, -hz],
            [-hx, -hy, -hz],
            [-hx, hy, -hz],
            [hx, hy, -hz],
            [0.0, 0.0, -1.0],
        ),
        (
            [hx, -hy, hz],
            [hx, -hy, -hz],
            [hx, hy, -hz],
            [hx, hy, hz],
            [1.0, 0.0, 0.0],
        ),
        (
            [-hx, -hy, -hz],
            [-hx, -hy, hz],
            [-hx, hy, hz],
            [-hx, hy, -hz],
            [-1.0, 0.0, 0.0],
        ),
        (
            [-hx, hy, hz],
            [hx, hy, hz],
            [hx, hy, -hz],
            [-hx, hy, -hz],
            [0.0, 1.0, 0.0],
        ),
        (
            [-hx, -hy, -hz],
            [hx, -hy, -hz],
            [hx, -hy, hz],
            [-hx, -hy, hz],
            [0.0, -1.0, 0.0],
        ),
    ];
    let bump = |v: [f32; 3]| -> [f32; 3] {
        let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        let d = [
            (v[0] * 41.0) as i64,
            (v[1] * 43.0) as i64,
            (v[2] * 47.0) as i64,
        ];
        let mut h = 0xcbf2_9ce4_8422_2325u64 ^ seed;
        for x in d {
            h ^= x as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        let k = 1.0 + ((h >> 8) & 0xFF) as f32 / 255.0 * 0.5 - 0.15;
        [v[0] / len * k, v[1] / len * k, v[2] / len * k]
    };
    for (a, b, c, d, _) in faces {
        let a = bump(a);
        let b = bump(b);
        let c = bump(c);
        let d = bump(d);
        let n = norm3([
            (b[1] - a[1]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[1] - a[1]),
            (b[2] - a[2]) * (c[0] - a[0]) - (b[0] - a[0]) * (c[2] - a[2]),
            (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]),
        ]);
        m.push_tri((a, n), (b, n), (c, n));
        m.push_tri((a, n), (c, n), (d, n));
    }
    m
}

fn norm3(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > f32::MIN_POSITIVE {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        [0.0, 1.0, 0.0]
    }
}

fn trunk_mesh() -> MeshBuf {
    cylinder(0.14, 1.1, 8)
}

fn foliage_mesh() -> MeshBuf {
    let mut m = cone(0.95, 1.6, 8, 0.9);
    let upper = cone(0.62, 1.3, 8, 1.9);
    m.append_transformed(&upper, &identity());
    m
}

fn identity() -> camera::Mat4 {
    let mut m = [[0.0f32; 4]; 4];
    for (i, row) in m.iter_mut().enumerate() {
        row[i] = 1.0;
    }
    m
}

/// 营火:5 根木柴绕中心呈放射状(绕 y 旋转的细盒,抬离地面防 z-fight)。
fn campfire_mesh() -> MeshBuf {
    let mut m = MeshBuf::default();
    for i in 0..5 {
        let th = (i as f32) * std::f32::consts::TAU / 5.0;
        let log = box_mesh(0.62, 0.13, 0.13);
        // 平移到半径 0.26 处,再绕 y 旋转朝向中心。
        let cos = th.cos();
        let sin = th.sin();
        let mut model = identity();
        model[0][0] = cos;
        model[0][2] = sin;
        model[2][0] = -sin;
        model[2][2] = cos;
        model[0][3] = 0.26 * cos;
        model[2][3] = 0.26 * sin;
        model[1][3] = 0.07;
        m.append_transformed(&log, &model);
    }
    m
}

/// 帐篷:双坡面 + 两端山墙,基部 y=0。
fn tent_mesh() -> MeshBuf {
    let mut m = MeshBuf::default();
    let (w, h, d) = (0.9f32, 0.85f32, 1.1f32);
    // 两坡面(每面两三角),山墙两三角,背板一矩形的两三角。
    let apex_f = [0.0, h, d / 2.0];
    let apex_b = [0.0, h, -d / 2.0];
    let e_f_l = [-w, 0.0, d / 2.0];
    let e_f_r = [w, 0.0, d / 2.0];
    let e_b_l = [-w, 0.0, -d / 2.0];
    let e_b_r = [w, 0.0, -d / 2.0];
    let n_front = norm3([0.0, w, h]);
    let n_back = norm3([0.0, w, -h]);
    m.push_tri((e_f_l, n_front), (e_f_r, n_front), (apex_f, n_front));
    m.push_tri((e_b_r, n_back), (e_b_l, n_back), (apex_b, n_back));
    let n_left = norm3([-h, 0.0, 0.0]);
    let n_right = norm3([h, 0.0, 0.0]);
    m.push_tri((e_b_l, n_left), (e_f_l, n_left), (apex_f, n_left));
    m.push_tri((e_b_l, n_left), (apex_f, n_left), (apex_b, n_left));
    m.push_tri((e_f_r, n_right), (e_b_r, n_right), (apex_b, n_right));
    m.push_tri((e_f_r, n_right), (apex_b, n_right), (apex_f, n_right));
    let n_end = norm3([0.0, 0.0, -1.0]);
    m.push_tri((apex_b, n_end), (e_b_l, n_end), (e_b_r, n_end));
    m
}

/// 全图像素平铺(行主序),供双跑逐位比对。
fn pixels_of(img: &RenderedImage) -> Vec<u8> {
    let mut v = Vec::with_capacity((img.width * img.height * 4) as usize);
    for y in 0..img.height {
        for x in 0..img.width {
            v.extend_from_slice(&img.pixel(x, y));
        }
    }
    v
}

/// 六个渲染 part(材质即 part;世界空间顶点 + 兰伯特法线)。
fn mk_parts(b: &Parts) -> Vec<ScenePart<'_>> {
    vec![
        ScenePart {
            positions: &b.terrain.pos,
            normals: Some(&b.terrain.nrm),
            uvs: None,
            indices: &b.terrain.idx,
            base_color: [0.30, 0.40, 0.20, 1.0],
            metallic: 0.0,
            roughness: 0.95,
            base_texture: None,
            mr_texture: None,
        },
        ScenePart {
            positions: &b.trunks.pos,
            normals: Some(&b.trunks.nrm),
            uvs: None,
            indices: &b.trunks.idx,
            base_color: [0.40, 0.28, 0.18, 1.0],
            metallic: 0.0,
            roughness: 0.9,
            base_texture: None,
            mr_texture: None,
        },
        ScenePart {
            positions: &b.foliage.pos,
            normals: Some(&b.foliage.nrm),
            uvs: None,
            indices: &b.foliage.idx,
            base_color: [0.20, 0.44, 0.22, 1.0],
            metallic: 0.0,
            roughness: 0.9,
            base_texture: None,
            mr_texture: None,
        },
        ScenePart {
            positions: &b.rocks.pos,
            normals: Some(&b.rocks.nrm),
            uvs: None,
            indices: &b.rocks.idx,
            base_color: [0.55, 0.55, 0.58, 1.0],
            metallic: 0.0,
            roughness: 0.85,
            base_texture: None,
            mr_texture: None,
        },
        ScenePart {
            positions: &b.fires.pos,
            normals: Some(&b.fires.nrm),
            uvs: None,
            indices: &b.fires.idx,
            base_color: [0.32, 0.22, 0.14, 1.0],
            metallic: 0.0,
            roughness: 0.9,
            base_texture: None,
            mr_texture: None,
        },
        ScenePart {
            positions: &b.tents.pos,
            normals: Some(&b.tents.nrm),
            uvs: None,
            indices: &b.tents.idx,
            base_color: [0.74, 0.70, 0.62, 1.0],
            metallic: 0.0,
            roughness: 0.9,
            base_texture: None,
            mr_texture: None,
        },
    ]
}

struct Parts {
    terrain: MeshBuf,
    trunks: MeshBuf,
    foliage: MeshBuf,
    rocks: MeshBuf,
    fires: MeshBuf,
    tents: MeshBuf,
}

/// 24 位无压缩 BMP(自下而上存行、BGR、行宽 4 字节对齐;与 gltf_scene 同式)。
fn write_bmp(path: &str, img: &RenderedImage) -> std::io::Result<()> {
    let (w, h) = (img.width as usize, img.height as usize);
    let row = (w * 3 + 3) & !3;
    let size = 54 + row * h;
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
    out.extend_from_slice(&0u32.to_le_bytes()); // biSizeImage
    out.extend_from_slice(&0u32.to_le_bytes()); // biXPelsPerMeter
    out.extend_from_slice(&0u32.to_le_bytes()); // biYPelsPerMeter
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant
    for y in (0..h).rev() {
        for x in 0..w {
            let p = img.pixel(x as u32, y as u32);
            // BMP 逐像素字节序是 B,G,R(pixel() 给的 BGRA 直接取前三)。
            out.extend_from_slice(&[p[0], p[1], p[2]]);
        }
        out.resize(out.len() + (row - w * 3), 0);
    }
    std::fs::write(path, out)
}
