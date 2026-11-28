//! 真实场景搭建连续动画(scene batch 展示版):全屏飞越 + 逐步搭建叙事。
//!
//! ```text
//! cargo run -p oriera-agent --release --example scene_flythrough -- \
//!   --dir frames/ [--frames 160] [--size WxH] [--seed N]
//! ffmpeg -y -framerate 24 -i frames/frame_%04d.bmp \
//!   -c:v libx264 -pix_fmt yuv420p -crf 18 scene_flythrough.mp4
//! ```
//!
//! 四幕叙事(每幕 25%):
//! - **空旷**(bare):种子高度场地形,高轨道俯瞰——"一块空地";
//! - **生长**(grow):144 松树从远到近逐渐出现,相机穿林下降——"森林长出来";
//! - **扎营**(camp):岩石群→营火→帐篷依次落位,相机逼近营地——"安家了";
//! - **飞越**(orbit):全场景低空环绕——"这就是搭建好的世界"。
//!
//! 确定性:场景数据(地形/散布/布局)全链双跑逐位一致;动画帧序列在
//! 同设备双渲染逐像素一致(首帧与末帧+每 16 帧抽样比对)。

use oriera_agent::scene_planner::{plan, PlacementIntent, SceneIntent};
use oriera_asset::scatter::{scatter, ScatterSpec};
use oriera_asset::terrain::{Terrain, TerrainSpec};
use oriera_common::math::Quat;
use oriera_common::{Transform, Vec3};
use oriera_render::camera::{self, Camera};
use oriera_render::gpu::{HeadlessRenderer, RenderedImage, SceneEnv, ScenePart, SceneSky};
use oriera_render::instancing::batch_by_key;
use oriera_render::scene::{Bounds, SceneInstance};

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

// ── 网格基元(与 scene_build 同套,自包含)────────────────────────────────

#[derive(Default, Clone)]
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

    /// 带矩阵变换的追加(每实例烘焙一次,帧间只 memcpy)。
    fn append_transformed(&mut self, other: &MeshBuf, m: &camera::Mat4) {
        let base = self.pos.len() as u32;
        for (p, n) in other.pos.iter().zip(&other.nrm) {
            let wp = camera::transform(m, *p);
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

    /// 无变换拼接(帧间合批用——预烘焙数据直接 memcpy)。
    fn append_raw(&mut self, other: &MeshBuf) {
        let base = self.pos.len() as u32;
        self.pos.extend_from_slice(&other.pos);
        self.nrm.extend_from_slice(&other.nrm);
        for i in &other.idx {
            self.idx.push(base + i);
        }
    }
}

fn norm3(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > f32::MIN_POSITIVE {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        [0.0, 1.0, 0.0]
    }
}

fn identity() -> camera::Mat4 {
    let mut m = [[0.0f32; 4]; 4];
    for (i, row) in m.iter_mut().enumerate() {
        row[i] = 1.0;
    }
    m
}

type QuadFace = ([f32; 3], [f32; 3], [f32; 3], [f32; 3], [f32; 3]);

fn box_faces(w: f32, h: f32, d: f32) -> [QuadFace; 6] {
    let (hx, hy, hz) = (w / 2.0, h / 2.0, d / 2.0);
    [
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
    ]
}

fn box_mesh(w: f32, h: f32, d: f32) -> MeshBuf {
    let mut m = MeshBuf::default();
    for (a, b, c, d, n) in box_faces(w, h, d) {
        m.push_tri((a, n), (b, n), (c, n));
        m.push_tri((a, n), (c, n), (d, n));
    }
    m
}

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

fn rock_mesh(seed: u64) -> MeshBuf {
    let mut m = MeshBuf::default();
    let (hx, hy, hz) = (0.8f32, 0.6f32, 0.8f32);
    let mut bump = |v: [f32; 3]| -> [f32; 3] {
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
    for (a, b, c, d, _) in box_faces(hx * 2.0, hy * 2.0, hz * 2.0) {
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

fn trunk_mesh() -> MeshBuf {
    cylinder(0.14, 1.1, 8)
}

fn foliage_mesh() -> MeshBuf {
    let mut m = cone(0.95, 1.6, 8, 0.9);
    let upper = cone(0.62, 1.3, 8, 1.9);
    m.append_transformed(&upper, &identity());
    m
}

fn campfire_mesh() -> MeshBuf {
    let mut m = MeshBuf::default();
    for i in 0..5 {
        let th = (i as f32) * std::f32::consts::TAU / 5.0;
        let log = box_mesh(0.62, 0.13, 0.13);
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

fn tent_mesh() -> MeshBuf {
    let mut m = MeshBuf::default();
    let (w, h, d) = (0.9f32, 0.85f32, 1.1f32);
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

fn transform_to_camera(t: &Transform) -> camera::Mat4 {
    let g = t.to_matrix().to_cols_array_2d();
    let mut out = [[0.0f32; 4]; 4];
    for r in 0..4 {
        for c in 0..4 {
            out[r][c] = g[c][r];
        }
    }
    out
}

/// 仓库 LCG 约定(MMIX 常数)。
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

// ── 相机动画(关键帧插值)────────────────────────────────────────────────

struct CamKey {
    t: f32,
    angle_deg: f32,
    radius: f32,
    height: f32,
    look_h: f32,
}

const CAM_KEYS: &[CamKey] = &[
    CamKey {
        t: 0.00,
        angle_deg: 0.0,
        radius: 75.0,
        height: 48.0,
        look_h: 3.0,
    },
    CamKey {
        t: 0.25,
        angle_deg: 95.0,
        radius: 55.0,
        height: 28.0,
        look_h: 2.0,
    },
    CamKey {
        t: 0.50,
        angle_deg: 185.0,
        radius: 28.0,
        height: 12.0,
        look_h: 1.0,
    },
    CamKey {
        t: 0.75,
        angle_deg: 275.0,
        radius: 14.0,
        height: 4.5,
        look_h: 0.5,
    },
    CamKey {
        t: 1.00,
        angle_deg: 360.0,
        radius: 10.0,
        height: 2.8,
        look_h: 0.3,
    },
];

fn smoothstep(x: f32) -> f32 {
    x * x * (3.0 - 2.0 * x)
}

fn camera_at(t: f32, center: [f32; 2], terrain: &Terrain, width: u32, height: u32) -> Camera {
    // 找到 t 所在的关键帧对
    let mut i = 0;
    while i < CAM_KEYS.len() - 2 && t > CAM_KEYS[i + 1].t {
        i += 1;
    }
    let k0 = &CAM_KEYS[i];
    let k1 = &CAM_KEYS[i + 1];
    let local = (t - k0.t) / (k1.t - k0.t);
    let s = smoothstep(local.clamp(0.0, 1.0));

    let angle = (k0.angle_deg + (k1.angle_deg - k0.angle_deg) * s).to_radians();
    let radius = k0.radius + (k1.radius - k0.radius) * s;
    let cam_h = k0.height + (k1.height - k0.height) * s;
    let look_h = k0.look_h + (k1.look_h - k0.look_h) * s;

    let eye_x = center[0] + radius * angle.cos();
    let eye_z = center[1] + radius * angle.sin();
    let eye_y = terrain.sample_bilinear(eye_x, eye_z) + cam_h;

    Camera::look_at(
        [eye_x, eye_y, eye_z],
        [
            center[0],
            terrain.sample_bilinear(center[0], center[1]) + look_h,
            center[1],
        ],
        [0.0, 1.0, 0.0],
        50.0,
        width,
        height,
        0.5,
        500.0,
    )
}

// ── 可见性(叙事时间轴)───────────────────────────────────────────────────

fn visible_count(t: f32, start: f32, end: f32, total: usize) -> usize {
    if total == 0 {
        return 0;
    }
    if t < start {
        return 0;
    }
    if t >= end {
        return total;
    }
    ((((t - start) / (end - start)) * total as f32) as usize).min(total)
}

// ── 主流程 ─────────────────────────────────────────────────────────────

struct Parts {
    terrain: MeshBuf,
    trunks: MeshBuf,
    foliage: MeshBuf,
    rocks: MeshBuf,
    fires: MeshBuf,
    tents: MeshBuf,
}

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

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let frame_dir = get("--dir").unwrap_or_else(|| "frames".into());
    let total_frames: usize = get("--frames").and_then(|s| s.parse().ok()).unwrap_or(160);
    let (width, height) = match get("--size") {
        Some(s) => {
            let (w, h) = s.split_once('x').ok_or("--size expects WxH")?;
            (
                w.parse::<u32>().map_err(|e| format!("{e}"))?,
                h.parse::<u32>().map_err(|e| format!("{e}"))?,
            )
        }
        None => (960, 540),
    };
    let seed: u64 = get("--seed")
        .and_then(|s| s.parse().ok())
        .unwrap_or(20261128);

    self_check_conventions();
    std::fs::create_dir_all(&frame_dir).map_err(|e| format!("{frame_dir}: {e}"))?;

    // ── 场景构建(一次性,与 scene_build 同链路)──────────────────────────
    let t0 = std::time::Instant::now();
    let spec = TerrainSpec::new(160, 160, 1.0, seed, 5, 7.0);
    let terrain = Terrain::generate(spec);
    let terrain_mesh = terrain.build_mesh(1);
    let terrain_normals: Vec<[f32; 3]> = terrain_mesh
        .positions
        .iter()
        .map(|p| terrain.normal_at(p[0], p[2]))
        .collect();
    let center = [80.0f32, 80.0f32];

    let area = 159.0 * 159.0;
    let sspec = ScatterSpec::new(150.0 * 100.0 / area, seed ^ 0x5EED, 0.8, 1.5, 32.0);
    let mut pines = scatter(&terrain, &sspec);
    pines.truncate(150);
    // 松树按离营地距离降序(远的先出现——营地区域后清出来)
    pines.sort_by(|a, b| {
        let da = (a.translation.x - center[0]).powi(2) + (a.translation.z - center[1]).powi(2);
        let db = (b.translation.x - center[0]).powi(2) + (b.translation.z - center[1]).powi(2);
        db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal)
    });

    let intent = SceneIntent {
        area_half: 10.0,
        seed: seed ^ 0xCAFE,
        placements: vec![
            PlacementIntent {
                asset_name: "rock".into(),
                count: 14,
                min_separation: 3.0,
                anchor: None,
            },
            PlacementIntent {
                asset_name: "campfire".into(),
                count: 1,
                min_separation: 0.0,
                anchor: Some(([0.0; 3], 0.0)),
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
    let layout = plan(&intent, &available).map_err(|e| e.to_string())?;
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

    // ── 预烘焙:每实例独立 MeshBuf(帧间只拼接,零重算)──────────────────
    let pine_trunk_tpl = trunk_mesh();
    let pine_foliage_tpl = foliage_mesh();
    let rock_tpl = rock_mesh(seed ^ 0x70C5);
    let fire_tpl = campfire_mesh();
    let tent_tpl = tent_mesh();

    let terrain_buf = MeshBuf {
        pos: terrain_mesh.positions.clone(),
        nrm: terrain_normals,
        idx: terrain_mesh.indices,
    };
    let pine_bakes: Vec<(MeshBuf, MeshBuf)> = pines
        .iter()
        .map(|tf| {
            let m = transform_to_camera(tf);
            let mut trk = MeshBuf::default();
            trk.append_transformed(&pine_trunk_tpl, &m);
            let mut fol = MeshBuf::default();
            fol.append_transformed(&pine_foliage_tpl, &m);
            (trk, fol)
        })
        .collect();
    let lcg_unused = Lcg(seed ^ 0xBADC0DE);
    let _ = lcg_unused;
    let item_bakes: Vec<(String, MeshBuf)> = campsite
        .iter()
        .map(|(name, tf)| {
            let mut tf = *tf;
            if name == "rock" {
                let s = 0.5 + Lcg(seed ^ (tf.translation.x.to_bits() as u64)).next_f32() * 0.7;
                tf.scale = Vec3::splat(s);
            }
            let m = transform_to_camera(&tf);
            let buf = match name.as_str() {
                "rock" => {
                    let mut b = MeshBuf::default();
                    b.append_transformed(&rock_tpl, &m);
                    b
                }
                "campfire" => {
                    let mut b = MeshBuf::default();
                    b.append_transformed(&fire_tpl, &m);
                    b
                }
                _ => {
                    let mut b = MeshBuf::default();
                    b.append_transformed(&tent_tpl, &m);
                    b
                }
            };
            (name.clone(), buf)
        })
        .collect();
    println!(
        "scene built | terrain {} verts | {} pines | {} campsite items | {:.1} ms",
        terrain_mesh.positions.len(),
        pines.len(),
        campsite.len(),
        t0.elapsed().as_secs_f64() * 1e3
    );

    // ── 渲染循环 ─────────────────────────────────────────────────────────
    let renderer = HeadlessRenderer::new().map_err(|e| e.to_string())?;
    println!("adapter: {}", renderer.adapter_info());
    let env = SceneEnv {
        clear: [0.55, 0.72, 0.98, 1.0],
        light_dir: norm3([-0.55, 0.5, -0.4]),
        light_intensity: 1.7,
        sky: Some(oriera_render::gpu::SceneSky {
            sun_dir: norm3([-0.4, 0.62, -0.3]),
            horizon_tint: [0.52, 0.65, 0.88],
            zenith_tint: [0.18, 0.34, 0.78],
            ground_tint: [0.55, 0.58, 0.62],
            sun_radiance: 26.0,
            sun_disc_deg: 2.2,
            overcast: 0.0,
        }),
    };

    let t1 = std::time::Instant::now();
    for frame in 0..total_frames {
        let t = frame as f32 / (total_frames - 1).max(1) as f32;
        let cam = camera_at(t, center, &terrain, width, height);

        // 可见性(叙事时间轴)
        let nv = visible_count(t, 0.15, 0.52, pine_bakes.len());
        let nr = visible_count(
            t,
            0.50,
            0.66,
            campsite.iter().filter(|(n, _)| n == "rock").count(),
        );
        let nf = visible_count(t, 0.68, 0.72, 1);
        let nt = visible_count(t, 0.76, 0.80, 1);

        // 帧级合批:地形(全量) + 可见实例拼接
        let mut trunks = MeshBuf::default();
        let mut foliage = MeshBuf::default();
        for (trk, fol) in pine_bakes.iter().take(nv) {
            trunks.append_raw(trk);
            foliage.append_raw(fol);
        }
        let mut rocks = MeshBuf::default();
        let mut fires = MeshBuf::default();
        let mut tents = MeshBuf::default();
        let mut ri = 0usize;
        for (name, buf) in &item_bakes {
            match name.as_str() {
                "rock" => {
                    if ri < nr {
                        rocks.append_raw(buf);
                        ri += 1;
                    }
                }
                "campfire" => {
                    if nf > 0 {
                        fires.append_raw(buf);
                    }
                }
                "tent" => {
                    if nt > 0 {
                        tents.append_raw(buf);
                    }
                }
                _ => {}
            }
        }
        let parts_buf = Parts {
            terrain: terrain_buf.clone(),
            trunks,
            foliage,
            rocks,
            fires,
            tents,
        };
        let parts = mk_parts(&parts_buf);
        let img = renderer
            .render_scene_views_env(&parts, &cam, &env)
            .map_err(|e| e.to_string())?;
        let path = format!("{frame_dir}/frame_{frame:04}.bmp");
        write_bmp(&path, &img).map_err(|e| format!("{path}: {e}"))?;
        if frame % 40 == 0 || frame == total_frames - 1 {
            println!(
                "frame {frame:>4}/{total_frames} | t={t:.3} | pines={nv} rocks={nr} fire={nf} tent={nt} | {:.1} ms cumulative",
                t1.elapsed().as_secs_f64() * 1e3
            );
        }
    }
    println!(
        "rendered {total_frames} frames in {:.1} s | dir: {frame_dir}",
        t1.elapsed().as_secs_f64()
    );
    println!(
        "next: ffmpeg -y -framerate 24 -i {frame_dir}/frame_%04d.bmp -c:v libx264 -pix_fmt yuv420p -crf 18 scene_flythrough.mp4"
    );
    Ok(())
}

fn self_check_conventions() {
    let t = Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2));
    let m = transform_to_camera(&t);
    let p = camera::transform(&m, [1.0, 0.0, 0.0]);
    assert!(
        ((p[0] - 0.0).abs() < 1e-5) && ((p[2] - (-1.0)).abs() < 1e-5),
        "glam↔camera 矩阵约定自检失败: {p:?}"
    );
}

/// 24 位无压缩 BMP。
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
    for _ in 0..6 {
        out.extend_from_slice(&0u32.to_le_bytes());
    }
    for y in (0..h).rev() {
        for x in 0..w {
            let p = img.pixel(x as u32, y as u32);
            out.extend_from_slice(&[p[0], p[1], p[2]]);
        }
        out.resize(out.len() + (row - w * 3), 0);
    }
    std::fs::write(path, out)
}
