//! L5 可见性剔除与 LOD（feature `cull`，默认关）——真实场景批次 WP3。
//!
//! 输入一列 [`SceneInstance`](crate::scene::SceneInstance)（世界空间包围球已由
//! [`crate::scene::Bounds::transformed`] 按模型矩阵放大成保守上界，本模块以
//! `bounds` 为准、不重算模型），输出"视锥内且未超距"的实例下标
//! （[`CullResult::visible`]，恒为输入顺序的原下标）与逐实例 LOD 档
//! （[`CullResult::lod`]，与 `visible` 对齐，0=最近档）。纯 CPU、headless
//! 可测、零新增依赖；消费方是合批器（WP4）——先剔后合。
//!
//! # 视锥平面推导（Gribb–Hartmann；行主序 + WebGPU 深度域）
//!
//! [`crate::camera`] 约定行主序存储、列向量作用：`clip_i = Σ_j m[i][j]·v_j`。
//! 因此**矩阵第 i 行本身就是裁剪坐标第 i 分量的系数向量**，平面抽取只是"行"
//! 的线性组合——列主序库里那步著名的"取转置的列"在这里不存在，这是照搬
//! 教科书代码最容易错的第一处。第二处是深度域：透视除法后本仓库
//! **z_ndc ∈ [0, 1]（WebGPU；GL 是 [-1, 1]）**，六个半空间不等式为
//!
//! ```text
//! left   : x + w ≥ 0   = 行0 + 行3      right  : -x + w ≥ 0  = 行3 - 行0
//! bottom : y + w ≥ 0   = 行1 + 行3      top    : -y + w ≥ 0  = 行3 - 行1
//! near   : z ≥ 0       = 行2            far    : -z + w ≥ 0  = 行3 - 行2
//! ```
//!
//! **near 一行就是 z∈[0,1] 与 GL 的全部差别**：GL 教材写 `near = 行3 + 行2`
//! （对应 z_ndc = −1 的平面）；这里近平面是 z_c = 0，即第 2 行本身。抄错会把
//! 近平面推到"更远一倍"的位置，近处整片几何被误剔。对 w ≤ 0（相机背后/位于
//! 镜头平面）的点：左右平面对要求 `x ≥ -w > 0` 且 `x ≤ w < 0`，自相矛盾，必被
//! 拒绝——六平面合起来恰好刻出截头棱锥，无需额外的 "w > 0" 闸（w = 0 的点只有
//! 裁剪原点这一退化位置能通过，按保守保留）。
//!
//! 平面按 `[left, right, bottom, top, near, far]` 存成 `[a, b, c, d]`，法向
//! `(a, b, c)` 朝内（指向可见半空间），构造时除以 `‖(a,b,c)‖` 归一，于是平面对
//! 点的带符号距离就是 `a·x + b·y + c·z + d`。`‖(a,b,c)‖` 为 0 或非有限（畸形
//! 矩阵）时保留原系数：距离语义失效，但行为仍确定。
//!
//! # 剔除与 LOD 的边界归属（全部写死，有测试钉住）
//!
//! - 视锥：六平面**全部**满足 `带符号距离 ≥ -半径` 才可见；任一平面
//!   `< -半径` 剔。恰切（`= -半径`）与跨界一律保留——剔除必须保守，
//!   像素级裁剪在 GPU。
//! - 距离：`dist = ‖center − camera_pos‖`（球心距，**不含**球半径）；
//!   `dist > max_distance` 剔，`== max_distance` 保留。
//! - LOD：比值 `ratio = radius / dist`；`ratio ≥ thresholds[0]` → 档 0，
//!   `ratio ≥ thresholds[1]` → 档 1，否则档 2——**恰等阈值归更近档**；
//!   `dist = 0`（相机在球心）按档 0。阈值按给定顺序使用、不重排不校验
//!   （约定 `thresholds[0] ≥ thresholds[1]`）。
//! - 统计口径：逐实例先视锥后距离，双双失败只记 [`CullStats::frustum_rejected`]；
//!   恒有 `tested = visible.len() + frustum_rejected + distance_rejected`。
//! - 非有限输入：NaN 球心按"保留 + 档 2"处理（所有 IEEE 比较为假）；
//!   `max_distance = +inf` 永不距离剔、`< 0` 全剔。这里不校验、不 panic，
//!   调用方应喂有限数据。
//!
//! # 诚实边界
//!
//! - 包围球沿用 [`crate::scene::Bounds`] 的保守口径，球测试天然再放大：
//!   视锥边缘的大物体即使本体不可见也会留下（可用 k-DOP/包围盒收敛，不做）。
//! - 距离按球心距：大物体在 `max_distance` 附近比"表面距"口径略早消失。
//! - 只做视锥 + 距离：没有遮挡剔除（被墙挡住的照画）、没有实例层级、
//!   没有时域平滑（LOD 抖动由调用方处理）。
//! - LOD 只给档位编号，不给切换迟滞——那是上层状态机的活。
//! - 确定性：无随机、无哈希迭代序、无环境依赖；同输入两次跑逐位一致。

use crate::camera::Mat4;
use crate::scene::{Bounds, SceneInstance};

/// 六平面视锥（世界空间，法向朝内、已归一）。由 [`Frustum::from_view_proj`]
/// 从 `view_proj` 抽取；平面序 `[left, right, bottom, top, near, far]`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frustum {
    planes: [[f32; 4]; 6],
}

impl Frustum {
    /// Gribb–Hartmann 平面抽取。`m` 是 `projection * view`（[`crate::camera`]
    /// 的行主序、列向量约定），配合 **WebGPU 深度域 z_ndc ∈ [0, 1]**；
    /// 推导见模块文档。正交矩阵同样适用（w ≡ 1 的"盒子"视锥）。
    pub fn from_view_proj(m: &Mat4) -> Self {
        let add = |a: &[f32; 4], b: &[f32; 4]| [a[0] + b[0], a[1] + b[1], a[2] + b[2], a[3] + b[3]];
        let sub = |a: &[f32; 4], b: &[f32; 4]| [a[0] - b[0], a[1] - b[1], a[2] - b[2], a[3] - b[3]];
        Self {
            planes: [
                normalize_plane(add(&m[0], &m[3])), // left:   x + w ≥ 0
                normalize_plane(sub(&m[3], &m[0])), // right:  w - x ≥ 0
                normalize_plane(add(&m[1], &m[3])), // bottom: y + w ≥ 0
                normalize_plane(sub(&m[3], &m[1])), // top:    w - y ≥ 0
                normalize_plane(m[2]),              // near:   z ≥ 0（WebGPU）
                normalize_plane(sub(&m[3], &m[2])), // far:    w - z ≥ 0
            ],
        }
    }

    /// 包围球与视锥是否（可能）相交。保守判定：六个带符号距离全部
    /// `≥ -radius` 才算不相交的否证失败——恰切（`= -radius`）与跨界一律
    /// 可见，剔除绝不吞掉压线的几何。
    pub fn intersects_sphere(&self, b: &Bounds) -> bool {
        let [cx, cy, cz] = b.center;
        let r = b.radius;
        for p in &self.planes {
            let d = p[0] * cx + p[1] * cy + p[2] * cz + p[3];
            if d < -r {
                return false;
            }
        }
        true
    }
}

/// 单平面归一：除以法向长度，使 `[a,b,c,d]` 直接给出带符号距离。
/// 法向长度为 0 或非有限（畸形矩阵）时原样返回——行为确定、语义失效。
fn normalize_plane(p: [f32; 4]) -> [f32; 4] {
    let n = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
    if n.is_finite() && n > 0.0 {
        [p[0] / n, p[1] / n, p[2] / n, p[3] / n]
    } else {
        p
    }
}

/// 剔除参数。`camera_pos` 是相机世界坐标（[`crate::camera::Camera::eye_pos`]
/// 可直接喂）；`max_distance` 与场景同单位（球心距口径）；`lod_thresholds`
/// 是"包围球半径 / 相机距离"比值的两个边界 `[t0, t1]`，约定 `t0 ≥ t1`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CullParams {
    pub camera_pos: [f32; 3],
    pub max_distance: f32,
    pub lod_thresholds: [f32; 2],
}

/// 逐次剔除的计数。口径见模块文档：先视锥后距离，二者之和不重叠。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CullStats {
    pub tested: u32,
    pub frustum_rejected: u32,
    pub distance_rejected: u32,
}

/// 剔除结果：`visible` 是输入顺序的原下标，`lod` 与之逐位对齐
/// （0=最近档，三档，边界归属见模块文档）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CullResult {
    pub visible: Vec<u32>,
    pub lod: Vec<u8>,
    pub stats: CullStats,
}

/// 视锥 + 距离剔除与三档 LOD。逐实例独立判定：先视锥（[`Frustum::intersects_sphere`]），
/// 再距离，幸存者按比值选档。输出顺序只取决于输入顺序；同输入两次跑逐位一致。
pub fn cull(insts: &[SceneInstance], frustum: &Frustum, params: &CullParams) -> CullResult {
    let mut res = CullResult::default();
    for (i, inst) in insts.iter().enumerate() {
        res.stats.tested += 1;
        if !frustum.intersects_sphere(&inst.bounds) {
            res.stats.frustum_rejected += 1;
            continue;
        }
        let [cx, cy, cz] = inst.bounds.center;
        let dx = cx - params.camera_pos[0];
        let dy = cy - params.camera_pos[1];
        let dz = cz - params.camera_pos[2];
        let dist = (dx * dx + dy * dy + dz * dz).sqrt();
        if dist > params.max_distance {
            res.stats.distance_rejected += 1;
            continue;
        }
        res.visible.push(i as u32);
        res.lod
            .push(lod_tier(inst.bounds.radius, dist, params.lod_thresholds));
    }
    res
}

/// 三档选档：`ratio = radius / dist`，恰等阈值归更近档；`dist = 0`（相机在
/// 球心）视作最近；NaN 比值（非有限输入）落到最远档。
fn lod_tier(radius: f32, dist: f32, thresholds: [f32; 2]) -> u8 {
    let ratio = if dist == 0.0 {
        f32::INFINITY
    } else {
        radius / dist
    };
    if ratio >= thresholds[0] {
        0
    } else if ratio >= thresholds[1] {
        1
    } else {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::{mul, perspective, view_look_at, Camera};

    // 平面槽位（与模块文档的存储序一致）。
    const LEFT: usize = 0;
    const RIGHT: usize = 1;
    const BOTTOM: usize = 2;
    const TOP: usize = 3;
    const NEAR: usize = 4;
    const FAR: usize = 5;

    const IDENT: Mat4 = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];

    /// 相机在原点看 -z、fov 90°、纵横比 1 的 view_proj（视空间 = 世界空间）。
    /// fov 90° → tan(45°) = 1，near=1/far=3 时投影矩阵各行都是二进制精确值。
    fn vp_fov90(near: f32, far: f32) -> Mat4 {
        let view = view_look_at([0.0; 3], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]);
        mul(
            &perspective(std::f32::consts::FRAC_PI_2, 1.0, near, far),
            &view,
        )
    }

    /// 正交风格的"盒子"裁剪矩阵：x, y ∈ [-20, 20]、z ∈ [0, 20]、w ≡ 1。
    /// 六平面法向全是 ±单轴，距离与恰切边界可逐位精确计算。
    fn box_view_proj() -> Mat4 {
        [
            [0.05, 0.0, 0.0, 0.0],
            [0.0, 0.05, 0.0, 0.0],
            [0.0, 0.0, 0.05, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    /// 单位模型矩阵 + 指定世界包围球的实例。
    fn inst(center: [f32; 3], radius: f32) -> SceneInstance {
        SceneInstance::new(0, 0, IDENT, Bounds::new(center, radius))
    }

    fn params(cam: [f32; 3], max_distance: f32, lod_thresholds: [f32; 2]) -> CullParams {
        CullParams {
            camera_pos: cam,
            max_distance,
            lod_thresholds,
        }
    }

    #[test]
    fn webgpu_near_plane_is_row2_and_box_planes_normalize_exactly() {
        // fov 90°、near 1、far 3:第 2 行 = [0,0,-1.5,-1.5],第 3 行 = [0,0,-1,0]。
        let f = Frustum::from_view_proj(&vp_fov90(1.0, 3.0));
        assert_eq!(
            f.planes[NEAR],
            [0.0, 0.0, -1.0, -1.0],
            "near 必须是行 2(z_ndc=0);GL 的 行3+行2 在这里会得到别的值"
        );
        assert_eq!(f.planes[FAR], [0.0, 0.0, 1.0, 3.0]);
        // 盒子矩阵的六平面全部精确(0.05 的归一恰好回到 ±1 与 20)。
        let b = Frustum::from_view_proj(&box_view_proj());
        assert_eq!(b.planes[LEFT], [1.0, 0.0, 0.0, 20.0]);
        assert_eq!(b.planes[RIGHT], [-1.0, 0.0, 0.0, 20.0]);
        assert_eq!(b.planes[BOTTOM], [0.0, 1.0, 0.0, 20.0]);
        assert_eq!(b.planes[TOP], [0.0, -1.0, 0.0, 20.0]);
        assert_eq!(b.planes[NEAR], [0.0, 0.0, 1.0, 0.0]);
        assert_eq!(b.planes[FAR], [0.0, 0.0, -1.0, 20.0]);
    }

    #[test]
    fn spheres_inside_outside_and_straddling() {
        let f = Frustum::from_view_proj(&vp_fov90(1.0, 3.0));
        // 视锥:z_view ∈ [-3,-1],|x|,|y| ≤ -z(fov 90°)。
        let at = |c: [f32; 3], r: f32| f.intersects_sphere(&Bounds::new(c, r));
        assert!(at([0.0, 0.0, -2.0], 0.5), "正中");
        assert!(at([0.5, 0.5, -2.0], 0.5), "四角内侧");
        assert!(!at([-10.0, 0.0, -2.0], 0.5), "左平面外");
        assert!(!at([0.0, 10.0, -2.0], 0.5), "上平面外");
        assert!(!at([0.0, 0.0, -5.0], 0.5), "远平面外");
        // 球心在左平面外 0.707、半径 2:跨界 → 保守可见。
        assert!(at([-3.0, 0.0, -2.0], 2.0), "跨界必须保留");
    }

    #[test]
    fn sphere_behind_near_plane_with_big_radius_is_visible() {
        let f = Frustum::from_view_proj(&vp_fov90(1.0, 3.0));
        let at = |c: [f32; 3], r: f32| f.intersects_sphere(&Bounds::new(c, r));
        // 球心在相机背后(z_view = +2,离近平面 3),半径 10 盖过近平面 → 可见。
        assert!(at([0.0, 0.0, 2.0], 10.0));
        // 相机恰在球心(距近平面 1 < 半径)→ 可见。
        assert!(at([0.0, 0.0, 0.0], 10.0));
        // 同位置的小球(r=0.5 < 1)整个躲进近平面之内 → 剔。
        assert!(!at([0.0, 0.0, 0.0], 0.5));
    }

    #[test]
    fn tangent_spheres_are_kept_conservatively() {
        let f = Frustum::from_view_proj(&box_view_proj());
        let at = |c: [f32; 3], r: f32| f.intersects_sphere(&Bounds::new(c, r));
        // 左平面 x = -20:恰切(带符号距离 = ∓半径)一律保留,再多 0.5 才剔。
        assert!(at([-21.0, 0.0, 10.0], 1.0), "外切:距离 = -半径,边界归保留");
        assert!(at([-19.0, 0.0, 10.0], 1.0), "内切:距离 = +半径");
        assert!(!at([-21.5, 0.0, 10.0], 1.0), "超出半径 0.5 → 剔");
        // 角点双切(左、下同时贴住)也保留。
        assert!(at([-21.0, -21.0, 10.0], 1.0));
    }

    #[test]
    fn near_and_far_tangency_is_kept() {
        let f = Frustum::from_view_proj(&vp_fov90(1.0, 3.0));
        let at = |c: [f32; 3], r: f32| f.intersects_sphere(&Bounds::new(c, r));
        // 近平面 z_view = -1:内切、外切都保留,再往眼睛挪 0.01 才剔。
        assert!(at([0.0, 0.0, -2.0], 1.0), "近平面内切(距 +1 = 半径)");
        assert!(at([0.0, 0.0, -0.5], 0.5), "近平面外切(距 -0.5 = -半径)");
        assert!(!at([0.0, 0.0, -0.49], 0.5), "越过外切点 → 剔");
        // 远平面 z_view = -3:在 far 之外贴住也保守保留。
        assert!(at([0.0, 0.0, -3.5], 0.5), "远平面外切");
        assert!(!at([0.0, 0.0, -3.6], 0.5), "整个越过 far → 剔");
    }

    #[test]
    fn distance_boundary_exactly_at_max_is_kept() {
        let f = Frustum::from_view_proj(&box_view_proj());
        let p = params([0.0; 3], 5.0, [0.25, 0.125]);
        let insts = [
            inst([3.0, 0.0, 4.0], 1.0), // ‖·‖ = 5 == max → 保留
            inst([6.0, 0.0, 8.0], 1.0), // 10 > 5 → 剔
            inst([3.0, 0.0, 0.0], 1.0), // 3 < 5 → 保留
            inst([0.0, 5.0, 0.0], 1.0), // 5 == max → 保留(任意方向同权)
        ];
        let r = cull(&insts, &f, &p);
        assert_eq!(r.visible, [0, 2, 3], "== max_distance 归保留");
        assert_eq!(r.lod, [1, 0, 1]);
        assert_eq!(
            r.stats,
            CullStats {
                tested: 4,
                frustum_rejected: 0,
                distance_rejected: 1,
            }
        );
    }

    #[test]
    fn degenerate_max_distance_is_pinned() {
        let f = Frustum::from_view_proj(&box_view_proj());
        let insts = [inst([0.0, 0.0, 5.0], 1.0), inst([0.0, 0.0, 0.0], 1.0)];
        let at = |max: f32| cull(&insts, &f, &params([0.0; 3], max, [0.25, 0.125]));
        // max = 0:只留与相机重合者(dist == 0 == max)。
        assert_eq!(at(0.0).visible, [1]);
        // max < 0:全部剔除(dist ≥ 0 > max)。
        let r = at(-1.0);
        assert_eq!(r.visible, Vec::<u32>::new());
        assert_eq!(r.stats.distance_rejected, 2);
        // max = +inf:永不距离剔。
        let r = at(f32::INFINITY);
        assert_eq!(r.visible, [0, 1]);
        assert_eq!(r.stats.distance_rejected, 0);
    }

    #[test]
    fn lod_threshold_boundaries_belong_to_the_nearer_tier() {
        let f = Frustum::from_view_proj(&box_view_proj());
        let p = params([0.0; 3], 100.0, [0.25, 0.125]);
        // 半径 1:d=4 → 比值 0.25 == t0 → 档 0;d=8 → 0.125 == t1 → 档 1;
        // d=16 → 0.0625 → 档 2;d=6 → 1/6 ≈ 0.167 → 档 1;相机在球心 → 档 0。
        let insts = [
            inst([4.0, 0.0, 0.0], 1.0),
            inst([8.0, 0.0, 0.0], 1.0),
            inst([16.0, 0.0, 0.0], 1.0),
            inst([6.0, 0.0, 0.0], 1.0),
            inst([0.0, 0.0, 0.0], 1.0),
        ];
        let r = cull(&insts, &f, &p);
        assert_eq!(r.visible, [0, 1, 2, 3, 4]);
        assert_eq!(r.lod, [0, 1, 2, 1, 0], "恰等阈值必须归更近档");
    }

    #[test]
    fn output_order_matches_input_and_stats_partition() {
        let f = Frustum::from_view_proj(&box_view_proj());
        let p = params([0.0; 3], 10.0, [0.25, 0.125]);
        let insts = [
            inst([-50.0, 0.0, 10.0], 1.0), // 视锥外(左)
            inst([1.0, 0.0, 5.0], 1.0),    // 留:比值 1/√26 ≈ 0.196 → 档 1
            inst([0.0, 50.0, 10.0], 1.0),  // 视锥外(上)
            inst([0.0, 0.0, 8.0], 2.5),    // 留:比值 2.5/8 = 0.3125 → 档 0
            inst([0.0, 1.0, 6.0], 1.0),    // 留:比值 ≈ 0.164 → 档 1
            inst([0.0, 0.0, 19.0], 1.0),   // 视锥内但超距(19 > 10)
        ];
        let r = cull(&insts, &f, &p);
        assert_eq!(r.visible, [1, 3, 4], "输出 = 输入顺序的原下标");
        assert_eq!(r.lod, [1, 0, 1], "lod 与 visible 逐位对齐");
        assert_eq!(
            r.stats,
            CullStats {
                tested: 6,
                frustum_rejected: 2,
                distance_rejected: 1,
            }
        );
        assert_eq!(
            r.stats.tested,
            r.visible.len() as u32 + r.stats.frustum_rejected + r.stats.distance_rejected
        );
    }

    #[test]
    fn double_run_is_bit_identical() {
        let vp = vp_fov90(0.5, 60.0);
        let f1 = Frustum::from_view_proj(&vp);
        let f2 = Frustum::from_view_proj(&vp);
        assert_eq!(f1, f2, "平面抽取两次逐位一致");
        let insts: Vec<SceneInstance> = (0..48u32)
            .map(|i| {
                let f = i as f32;
                SceneInstance::new(
                    i,
                    i % 4,
                    IDENT,
                    Bounds::new(
                        [f * 1.7 - 15.0, f * -0.9 + 8.0, -(f * 2.3 + 2.0)],
                        0.5 + (i % 5) as f32,
                    ),
                )
            })
            .collect();
        let p = params([0.0, 1.0, 0.0], 30.0, [0.2, 0.05]);
        let a = cull(&insts, &f1, &p);
        let b = cull(&insts, &f2, &p);
        assert_eq!(a, b, "同输入两跑逐位一致");
        assert_eq!(a.stats.tested, 48);
        assert!(
            a.stats.tested
                == a.visible.len() as u32 + a.stats.frustum_rejected + a.stats.distance_rejected
        );
    }

    #[test]
    fn frustum_agrees_with_camera_projection() {
        // 非单位视角的真实相机:平面判定与 Camera::project 的 NDC 盒子必须互洽。
        let cam = Camera::look_at(
            [2.0, 1.0, 7.0],
            [0.0, 0.0, -3.0],
            [0.0, 1.0, 0.0],
            55.0,
            96,
            64,
            0.5,
            40.0,
        );
        let f = Frustum::from_view_proj(&cam.view_proj());
        let min_plane = |p: [f32; 3]| -> f32 {
            let mut m = f32::INFINITY;
            for pl in &f.planes {
                m = m.min(pl[0] * p[0] + pl[1] * p[1] + pl[2] * p[2] + pl[3]);
            }
            m
        };
        let check = |p: [f32; 3]| {
            let m = min_plane(p);
            if m > 1e-3 {
                // 六平面全正 → 必有 w > 0 且 NDC 严格在盒子内。
                let n = cam.project(p).expect("严格在视锥内的点必须可投影");
                assert!(
                    n[0].abs() <= 1.0 && n[1].abs() <= 1.0 && (0.0..=1.0).contains(&n[2]),
                    "平面判内但 NDC 出盒:p={p:?} → {n:?}"
                );
            } else if m < -1e-3 {
                // 某平面严格为负 → 要么投不出来,要么 NDC 出盒。
                if let Some(n) = cam.project(p) {
                    assert!(
                        n[0].abs() > 1.0 || n[1].abs() > 1.0 || !(0.0..=1.0).contains(&n[2]),
                        "平面判外但 NDC 在盒内:p={p:?} → {n:?}"
                    );
                }
            }
            // |m| ≤ 1e-3 的骑线点跳过(浮点边界,两边都合法)。
        };
        for &dx in &[-4.0f32, -1.0, 0.0, 1.0, 4.0] {
            for &dy in &[-4.0f32, -1.0, 0.0, 1.0, 4.0] {
                for &dz in &[-4.0f32, -1.0, 0.0, 1.0, 4.0] {
                    check([dx, dy, -3.0 + dz]);
                }
            }
        }
        // 特位:注视点(正中)、far 之外、相机正后方。
        check([0.0, 0.0, -3.0]);
        check([0.0, 0.0, -50.0]);
        check([2.0, 1.0, 12.0]);
    }

    #[test]
    fn real_camera_culls_behind_and_above() {
        let cam = Camera::look_at(
            [0.0, 0.0, 5.0],
            [0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            60.0,
            64,
            64,
            0.1,
            50.0,
        );
        let f = Frustum::from_view_proj(&cam.view_proj());
        let p = params(cam.eye_pos(), 100.0, [0.25, 0.125]);
        let insts = [
            inst([0.0, 0.0, 0.0], 1.0),  // 正前 5 → 留,比值 0.2 → 档 1
            inst([0.0, 0.0, 10.0], 1.0), // 相机背后 → 视锥剔
            inst([0.0, 50.0, 0.0], 1.0), // 头顶 45(视场外)→ 视锥剔
        ];
        let r = cull(&insts, &f, &p);
        assert_eq!(r.visible, [0]);
        assert_eq!(r.lod, [1]);
        assert_eq!(
            r.stats,
            CullStats {
                tested: 3,
                frustum_rejected: 2,
                distance_rejected: 0,
            }
        );
    }

    #[test]
    fn non_finite_center_is_kept_at_farthest_lod() {
        let f = Frustum::from_view_proj(&box_view_proj());
        let p = params([0.0; 3], 10.0, [0.25, 0.125]);
        let insts = [inst([f32::NAN, 0.0, 5.0], 1.0)];
        let r = cull(&insts, &f, &p);
        // NaN 的所有比较为假:视锥不剔、距离不剔、比值落最远档。
        assert_eq!(r.visible, [0]);
        assert_eq!(r.lod, [2]);
        assert_eq!(
            r.stats,
            CullStats {
                tested: 1,
                frustum_rejected: 0,
                distance_rejected: 0,
            }
        );
    }

    #[test]
    fn empty_input_yields_empty_result() {
        let f = Frustum::from_view_proj(&box_view_proj());
        let r = cull(&[], &f, &params([0.0; 3], 1.0, [0.5, 0.1]));
        assert_eq!(r.visible, Vec::<u32>::new());
        assert!(r.lod.is_empty());
        assert_eq!(r.stats, CullStats::default());
    }
}
