//! L6 navigation backend (feature `nav`) — 凸多边形导航网格 + A* + 漏斗拉直。
//!
//! 对应 QY-Demo 拆解里第二档的「NavMesh 寻路」：NPC 自主穿城需要一条可复现的
//! 路线，而现有 `rigid3d` 只做关节物理，没有导航网格。本模块纯 CPU，可确定性
//! 测试，不碰 GPU。
//!
//! **契约**
//! - [`NavMesh`] 由调用方给的一组**凸**多边形建成；邻接靠"两个多边形共享一条
//!   边、端点顺序相反"推导，不需要额外输入。绕向自动规整为"正面积"方向，
//!   顶点重复的首尾点会被丢掉。
//! - [`NavMesh::find_path`] 先定位起终点所在的多边形（点不在岸上就投影到最近
//!   点），在多边形图上跑 A*（边权 = 质心间距，启发 = 质心到目标的直线距离），
//!   再用漏斗算法把走廊折线拉直。返回路径首点为起点、末点为终点。
//! - [`NavAgentSystem`] 是**运动学**移动器：自己写 `Position`，并把本帧真实位移
//!   折算成 `Velocity` 写回（读出来与实际位移一致）。**不要和物理积分器同时
//!   驱动同一批实体**——两个写者会互相覆盖，这一条没有互相协商的机制。
//! - **确定性**：只依赖输入顺序与浮点加减乘除，不使用 `HashMap` 迭代序；
//!   A* 的 tie-break 是"f 值相同的多边形下标小的先出"，因此同一输入两次跑
//!   逐位一致。[`path_fingerprint`] 给回执用（episode 指纹的导航段）。
//!
//! **诚实边界**
//! - 多边形必须凸：漏斗按凸性假设拉直，非凸多边形会在 [`NavMesh::from_polygons`]
//!   被拒（`NavError::NonConvex`），而不是悄悄切角。
//! - 坐标是 L3 的 2D 平面（[`Position`] 的 `x`/`y`，`y` 向下，与物理一致），
//!   没有高度、没有层间连接（楼梯/电梯要做成额外面片加手工接边，本步不做）。
//! - 没有动态障碍、没有群体避让。要避让就在上层写 `Velocity`，导航只负责路线。
//! - 邻接检测是 O(E²) 两两比边：几百个多边形够用，城市级要换空间索引。
//! - 走廊校验（[`NavMesh::segment_walkable`]）按固定步长采样，是"足够密"的
//!   近似，不是解析证明。
//!
//! 与既有子系统的关系：本模块不读 `Gravity`，只读 `TimeScale`（走法节奏跟世界
//! 调速走）；它排在物理之前或之后都行，但同一批实体只能有一个写者。

use crate::TimeScale;
use oriera_core::{FrameCtx, System};
use oriera_ecs::{Position, Velocity, World};
use std::cmp::Ordering;
use std::collections::BinaryHeap;

/// 顶点比较与共线判定的容差。单位是导航面片里的世界单位（米量级），
/// 取 1e-4 相当于亚毫米——比任何里程计噪声都小。
const EPS: f32 = 1e-4;

/// 建网格失败的原因。全部在**构建期**报，运行期 `find_path` 只返回 `None`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavError {
    /// 一个多边形都没给。
    NoPolygons,
    /// 第 `i` 个多边形顶点少于 3 个。
    TooFewVerts(usize),
    /// 第 `i` 个多边形面积塌陷成一条线。
    Degenerate(usize),
    /// 第 `i` 个多边形不是凸的（含自交、凹角）。
    NonConvex(usize),
    /// 第 `i` 个多边形的第 `k` 条边同时匹配了第 `j`、`m` 个多边形——共享边
    /// 匹配不唯一，拓扑有歧义。
    AmbiguousEdge(usize, usize, usize),
}

impl std::fmt::Display for NavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NavError::NoPolygons => write!(f, "no polygons given"),
            NavError::TooFewVerts(i) => write!(f, "polygon {i} has fewer than 3 vertices"),
            NavError::Degenerate(i) => write!(f, "polygon {i} has zero area"),
            NavError::NonConvex(i) => write!(f, "polygon {i} is not convex"),
            NavError::AmbiguousEdge(i, j, m) => {
                write!(f, "polygon {i} shares an edge with both {j} and {m}",)
            }
        }
    }
}

impl std::error::Error for NavError {}

/// 一条邻接记录：跨过本多边形的第 `edge` 条边，进入 `poly`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Neighbor {
    pub poly: usize,
    pub edge: usize,
}

/// 一个凸多边形。顶点按正面积方向（`y` 向下坐标里的逆时针）排列。
#[derive(Debug, Clone, PartialEq)]
pub struct NavPoly {
    verts: Vec<[f32; 2]>,
}

impl NavPoly {
    pub fn verts(&self) -> &[[f32; 2]] {
        &self.verts
    }
    pub fn len(&self) -> usize {
        self.verts.len()
    }
    pub fn is_empty(&self) -> bool {
        self.verts.is_empty()
    }
}

/// 面积的两倍（有符号）。正面积方向就是本模块统一后的绕向。
fn signed_double_area(v: &[[f32; 2]]) -> f32 {
    let mut s = 0.0;
    for i in 0..v.len() {
        let a = v[i];
        let b = v[(i + 1) % v.len()];
        s += a[0] * b[1] - b[0] * a[1];
    }
    s
}

fn sub(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

fn cross(o: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    let d = sub(a, b);
    (d[0] * d[0] + d[1] * d[1]).sqrt()
}

fn same_point(a: [f32; 2], b: [f32; 2]) -> bool {
    (a[0] - b[0]).abs() <= EPS && (a[1] - b[1]).abs() <= EPS
}

fn centroid(v: &[[f32; 2]]) -> [f32; 2] {
    let mut s = [0.0f32, 0.0];
    for p in v {
        s[0] += p[0];
        s[1] += p[1];
    }
    let n = v.len() as f32;
    [s[0] / n, s[1] / n]
}

/// 导航网格：一组凸多边形 + 推导出来的邻接表。
///
/// 结构一旦建好就不可变——改地形就重建。这保证 `find_path` 是纯函数式的
/// （只读网格 + 两个点），可以并发调用，也保证两次跑的路线逐位一致。
#[derive(Debug, Clone, PartialEq)]
pub struct NavMesh {
    polys: Vec<NavPoly>,
    centroids: Vec<[f32; 2]>,
    neigh: Vec<Vec<Neighbor>>,
}

impl NavMesh {
    /// 从多边形列表建网格。
    ///
    /// 输入容忍两件事：绕向（自动规整成正面积方向）、首尾重复的闭合点（丢掉）。
    /// 拒绝三件事：顶点少于 3、面积塌陷、非凸。共享边必须**整条重合**——
    /// 半条边重合不构成邻接（那是两个面片没对齐，属于建模错误）。
    pub fn from_polygons(input: &[Vec<[f32; 2]>]) -> Result<Self, NavError> {
        if input.is_empty() {
            return Err(NavError::NoPolygons);
        }
        let mut polys: Vec<NavPoly> = Vec::with_capacity(input.len());
        for (i, raw) in input.iter().enumerate() {
            let mut v = raw.clone();
            // 丢掉闭合点（首尾重合），再丢掉沿途重复点。
            if v.len() >= 2 && same_point(v[0], v[v.len() - 1]) {
                v.pop();
            }
            let mut dedup: Vec<[f32; 2]> = Vec::with_capacity(v.len());
            for p in v {
                if dedup.last().map(|q| same_point(*q, p)).unwrap_or(false) {
                    continue;
                }
                dedup.push(p);
            }
            if dedup.len() >= 3 && same_point(dedup[0], dedup[dedup.len() - 1]) {
                dedup.pop();
            }
            if dedup.len() < 3 {
                return Err(NavError::TooFewVerts(i));
            }
            let area2 = signed_double_area(&dedup);
            if area2.abs() <= 2.0 * EPS {
                return Err(NavError::Degenerate(i));
            }
            if area2 < 0.0 {
                dedup.reverse();
            }
            // 凸性：相邻三点的叉积同号（正面积方向下应为正或共线）。
            let n = dedup.len();
            for k in 0..n {
                let a = dedup[(k + n - 1) % n];
                let b = dedup[k];
                let c = dedup[(k + 1) % n];
                if cross(a, b, c) < -1e-3 {
                    return Err(NavError::NonConvex(i));
                }
            }
            polys.push(NavPoly { verts: dedup });
        }

        // 邻接：两两比边，找"端序相反、整条重合"的共享边。O(E²)，确定性。
        let mut neigh: Vec<Vec<Neighbor>> = vec![Vec::new(); polys.len()];
        for i in 0..polys.len() {
            let ni = polys[i].len();
            for k in 0..ni {
                let a = polys[i].verts[k];
                let b = polys[i].verts[(k + 1) % ni];
                let mut hits: Vec<(usize, usize)> = Vec::new();
                for (j, pj) in polys.iter().enumerate() {
                    if j == i {
                        continue;
                    }
                    let nj = pj.len();
                    for m in 0..nj {
                        let c = pj.verts[m];
                        let d = pj.verts[(m + 1) % nj];
                        if same_point(a, d) && same_point(b, c) {
                            hits.push((j, m));
                        }
                    }
                }
                match hits.len() {
                    0 => {}
                    1 => {
                        neigh[i].push(Neighbor {
                            poly: hits[0].0,
                            edge: k,
                        });
                    }
                    _ => {
                        return Err(NavError::AmbiguousEdge(i, hits[0].0, hits[1].0));
                    }
                }
            }
        }
        // 邻接表按多边形下标排序，避免依赖比较顺序之外的任何东西。
        for list in neigh.iter_mut() {
            list.sort_by_key(|n| n.poly);
        }

        let centroids: Vec<[f32; 2]> = polys.iter().map(|p| centroid(&p.verts)).collect();
        Ok(Self {
            polys,
            centroids,
            neigh,
        })
    }

    pub fn poly_count(&self) -> usize {
        self.polys.len()
    }

    pub fn poly(&self, i: usize) -> &NavPoly {
        &self.polys[i]
    }

    pub fn centroid(&self, i: usize) -> [f32; 2] {
        self.centroids[i]
    }

    pub fn neighbors(&self, i: usize) -> &[Neighbor] {
        &self.neigh[i]
    }

    /// 点落在哪个多边形里（含边界）。多个都算"在"时取下标最小的一个——
    /// 边界点归属唯一，路线才可复现。
    pub fn locate(&self, p: [f32; 2]) -> Option<usize> {
        (0..self.polys.len()).find(|&i| self.contains(i, p))
    }

    fn contains(&self, i: usize, p: [f32; 2]) -> bool {
        let v = &self.polys[i].verts;
        let n = v.len();
        let mut pos = false;
        let mut neg = false;
        for k in 0..n {
            let c = cross(v[k], v[(k + 1) % n], p);
            if c > EPS {
                pos = true;
            }
            if c < -EPS {
                neg = true;
            }
        }
        !(pos && neg)
    }

    /// 把任意点贴到网格上：返回最近点与它所属的多边形。点已在网格内时
    /// 原样返回（不挪动）。网格为空时返回 `None`（构造期已拒绝，这里只为
    /// 签名完整）。
    pub fn project(&self, p: [f32; 2]) -> Option<([f32; 2], usize)> {
        if let Some(i) = self.locate(p) {
            return Some((p, i));
        }
        let mut best: Option<(f32, [f32; 2], usize)> = None;
        for i in 0..self.polys.len() {
            let v = &self.polys[i].verts;
            let n = v.len();
            for k in 0..n {
                let q = closest_on_segment(p, v[k], v[(k + 1) % n]);
                let d = dist(p, q);
                // 严格小于才替换：同距离时保留下标更小的（确定性）。
                if best.map(|b| d < b.0 - EPS).unwrap_or(true) {
                    best = Some((d, q, i));
                }
            }
        }
        best.map(|(_, q, i)| (q, i))
    }
}

/// 线段 `ab` 上离 `p` 最近的点（含端点）。
fn closest_on_segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    let ab = sub(b, a);
    let len2 = ab[0] * ab[0] + ab[1] * ab[1];
    if len2 <= EPS * EPS {
        return a;
    }
    let t = ((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1]) / len2;
    let t = t.clamp(0.0, 1.0);
    [a[0] + ab[0] * t, a[1] + ab[1] * t]
}

/// A* 的堆节点。f 小的先出；f 相同时多边形下标小的先出——这条 tie-break
/// 是"同输入同路线"的唯一保障（否则堆内顺序由插入历史决定，而插入历史
/// 又取决于邻接表的顺序，读起来像是确定的，其实不是）。
#[derive(Debug, Clone, Copy)]
struct HeapNode {
    f: f32,
    idx: usize,
}

impl PartialEq for HeapNode {
    fn eq(&self, other: &Self) -> bool {
        self.f.total_cmp(&other.f) == Ordering::Equal && self.idx == other.idx
    }
}
impl Eq for HeapNode {}
impl Ord for HeapNode {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap 是最大堆：f 更小 → 优先级更高 → 返回 Greater。
        match self.f.total_cmp(&other.f) {
            Ordering::Less => Ordering::Greater,
            Ordering::Greater => Ordering::Less,
            Ordering::Equal => other.idx.cmp(&self.idx),
        }
    }
}
impl PartialOrd for HeapNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl NavMesh {
    /// 在多边形图上跑 A*，返回**已定向的传送门序列**（每项为 `(left, right)`）。
    ///
    /// 边权是相邻两个多边形的质心间距，启发是当前质心到目标多边形的质心距离
    /// ——欧氏距离满足一致性（不高于真实剩余代价），A* 因此返回最优走廊。
    fn route(&self, si: usize, ti: usize) -> Option<Vec<([f32; 2], [f32; 2])>> {
        let n = self.polys.len();
        if si == ti {
            return Some(Vec::new());
        }
        let h = |i: usize| dist(self.centroids[i], self.centroids[ti]);
        let mut g = vec![f32::INFINITY; n];
        let mut came: Vec<Option<(usize, usize)>> = vec![None; n];
        let mut closed = vec![false; n];
        let mut open = BinaryHeap::new();
        g[si] = 0.0;
        open.push(HeapNode { f: h(si), idx: si });
        while let Some(node) = open.pop() {
            if closed[node.idx] {
                continue;
            }
            if node.idx == ti {
                break;
            }
            closed[node.idx] = true;
            for nb in &self.neigh[node.idx] {
                let w = dist(self.centroids[node.idx], self.centroids[nb.poly]);
                let cand = g[node.idx] + w;
                if cand < g[nb.poly] - EPS {
                    g[nb.poly] = cand;
                    came[nb.poly] = Some((node.idx, nb.edge));
                    open.push(HeapNode {
                        f: cand + h(nb.poly),
                        idx: nb.poly,
                    });
                }
            }
        }
        came[ti]?;
        let mut chain = vec![ti];
        let mut cur = ti;
        while cur != si {
            let (prev, _edge) = came[cur]?;
            chain.push(prev);
            cur = prev;
        }
        chain.reverse();

        // 把"每步用哪条边"变成传送门，并按行进方向定 left/right。
        let mut portals = Vec::with_capacity(chain.len().saturating_sub(1));
        for k in 0..chain.len() - 1 {
            let prev = chain[k];
            let next = chain[k + 1];
            let (_, edge) = came[next]?;
            let nv = self.polys[prev].len();
            let a = self.polys[prev].verts[edge];
            let b = self.polys[prev].verts[(edge + 1) % nv];
            let dir = sub(self.centroids[next], self.centroids[prev]);
            // 左 = 行进方向"逆时针侧"（y 向下坐标系里即 x 减小的一侧）。符号定死
            // 在这一个地方：漏斗的两个收窄条件对左右不是对称的，翻一次就会拉直
            // 成穿墙直线——`every_leg_of_the_path_is_walkable` 就是钉这条的。
            let (left, right) = if cross([0.0, 0.0], dir, sub(a, self.centroids[prev])) < 0.0 {
                (a, b)
            } else {
                (b, a)
            };
            portals.push((left, right));
        }
        Some(portals)
    }

    /// 规划一条从 `from` 到 `to` 的路线。
    ///
    /// 起终点不在网格上时先投影到最近的多边形边界（不报错——NPC 轻微越界
    /// 是常态，硬报错会让上层抓到一堆无意义的 `None`）。两个点落在同一个
    /// 多边形时直接返回两点（凸多边形内直线必然可行）。不可达返回 `None`。
    pub fn find_path(&self, from: [f32; 2], to: [f32; 2]) -> Option<Vec<[f32; 2]>> {
        let (start, si) = self.project(from)?;
        let (goal, ti) = self.project(to)?;
        if si == ti {
            return Some(vec![start, goal]);
        }
        let portals = self.route(si, ti)?;
        let mut ps = portals;
        ps.push((goal, goal));
        Some(funnel(start, &ps, goal))
    }

    /// 线段是否整段可行（两端都在网格上，中途不穿出）。
    ///
    /// 按 0.25 个世界单位的固定步长采样——"足够密"的近似。用于测试与
    /// 上层自检（例如验收一段位移），不要拿它当精确几何谓词。
    pub fn segment_walkable(&self, a: [f32; 2], b: [f32; 2]) -> bool {
        let len = dist(a, b);
        let steps = ((len / 0.25).ceil() as usize).max(1);
        (0..=steps).all(|k| {
            let t = k as f32 / steps as f32;
            let p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
            match self.project(p) {
                Some((q, _)) => dist(p, q) <= EPS,
                None => false,
            }
        })
    }
}

/// 漏斗算法（simple stupid funnel）：把多边形走廊折线拉成最短的直线段序列。
///
/// `portals[i] = (left, right)`，最后一项是 `(goal, goal)`。输出含起点与终点；
/// 中间点是"必须贴着拐"的角点。
fn funnel(start: [f32; 2], portals: &[([f32; 2], [f32; 2])], goal: [f32; 2]) -> Vec<[f32; 2]> {
    let mut out = vec![start];
    if portals.is_empty() {
        out.push(goal);
        return out;
    }
    let mut apex = start;
    let mut left = portals[0].0;
    let mut right = portals[0].1;
    let mut left_i = 0usize;
    let mut right_i = 0usize;
    let mut i = 1usize;

    while i < portals.len() {
        let (l, r) = portals[i];

        // 右侧收窄
        if cross(apex, right, r) <= 0.0 {
            if cross(apex, left, r) > 0.0 {
                // 收到极限：左角点成为拐点，从该点重新开始
                out.push(left);
                apex = left;
                let base = portals[left_i];
                left = base.0;
                right = base.1;
                right_i = left_i;
                i = left_i + 1;
                continue;
            }
            right = r;
            right_i = i;
        }
        // 左侧收窄
        if cross(apex, left, l) >= 0.0 {
            if cross(apex, right, l) < 0.0 {
                out.push(right);
                apex = right;
                let base = portals[right_i];
                left = base.0;
                right = base.1;
                left_i = right_i;
                i = right_i + 1;
                continue;
            }
            left = l;
            left_i = i;
        }
        i += 1;
    }
    if !same_point(out[out.len() - 1], goal) {
        out.push(goal);
    }
    // 去掉零长度段（相邻重复点）。
    out.dedup_by(|a, b| same_point(*a, *b));
    out
}

/// 路线的确定性指纹（FNV-1a 64）。同输入两次跑同值；给 episode 回执用。
///
/// 只覆盖坐标位模式与点数：它证明"这一帧的路线和回放里那帧逐位相同"，不证明
/// 路线好。
pub fn path_fingerprint(path: &[[f32; 2]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    };
    feed(&(path.len() as u64).to_le_bytes());
    for p in path {
        feed(&p[0].to_bits().to_le_bytes());
        feed(&p[1].to_bits().to_le_bytes());
    }
    h
}

/// 行走意图：去哪、走多快、走到没到。挂在实体上，和 `Position` / `Velocity`
/// 一起由 [`NavAgentSystem`] 消费。
#[derive(Debug, Clone, PartialEq)]
pub struct NavAgent {
    /// 期望速度（世界单位/秒）。0 表示停着不动。
    pub speed: f32,
    /// 目标点（2D 平面）。`None` = 没有指令。
    pub goal: Option<[f32; 2]>,
    /// 已到达：踩完最后一个路点置真，直到下次 [`NavAgent::go_to`]。
    pub arrived: bool,
    /// 目标不在同一块网格上（不可达）。不会每帧重试，等新指令才清。
    pub unreachable: bool,
    /// 当前路线（首点为起点，末点为目标）。到达或重新规划后清空。
    pub path: Vec<[f32; 2]>,
    /// 下一个要走向的路点下标（0 是起点，所以从 1 开始）。
    pub cursor: usize,
    /// 累计规划次数（遥测：看得见"重新打算了几次"）。
    pub repaths: u32,
}

impl Default for NavAgent {
    fn default() -> Self {
        Self {
            speed: 10.0,
            goal: None,
            arrived: false,
            unreachable: false,
            path: Vec::new(),
            cursor: 0,
            repaths: 0,
        }
    }
}

impl NavAgent {
    pub fn new(speed: f32) -> Self {
        Self {
            speed,
            ..Default::default()
        }
    }

    /// 下一条指令。清掉上一条路线的全部状态（含 `arrived` / `unreachable`）。
    pub fn go_to(&mut self, goal: [f32; 2]) {
        self.goal = Some(goal);
        self.arrived = false;
        self.unreachable = false;
        self.path.clear();
        self.cursor = 0;
    }

    /// 停下并忘掉指令（不保留目标）。
    pub fn halt(&mut self) {
        self.goal = None;
        self.arrived = false;
        self.path.clear();
        self.cursor = 0;
    }
}

/// 导航子系统：给带 [`NavAgent`] 的实体规划路线并沿路线推进。
///
/// **运动学**：它自己写 `Position`。同一批实体不要再交给物理积分器驱动——
/// 两个写者会互相覆盖，谁后跑谁赢。要让物理管位移，就只读它的 `Velocity`
/// 并清掉 `NavAgent`。
///
/// 节奏跟 `TimeScale` 走：`dt_eff = ctx.dt * TimeScale::effective()`，世界冻住
/// 时 NPC 也不动（`dt_eff <= 0` 直接跳过整帧）。
pub struct NavAgentSystem {
    pub mesh: NavMesh,
}

impl NavAgentSystem {
    pub fn new(mesh: NavMesh) -> Self {
        Self { mesh }
    }
}

impl System<World> for NavAgentSystem {
    fn name(&self) -> &str {
        "nav"
    }

    fn run(&mut self, ctx: &FrameCtx, world: &mut World) {
        let scale = world
            .resource::<TimeScale>()
            .map(|t| t.effective())
            .unwrap_or(1.0);
        let dt = ctx.dt * scale;
        if dt <= 0.0 || !dt.is_finite() {
            return;
        }
        let targets: Vec<oriera_ecs::Entity> = world.query::<NavAgent>().map(|(e, _)| e).collect();
        for e in targets {
            let pos = match world.get::<Position>(e) {
                Some(p) => *p,
                None => continue,
            };
            let mut agent = match world.get::<NavAgent>(e) {
                Some(a) => a.clone(),
                None => continue,
            };
            let mut x = pos.x;
            let mut y = pos.y;

            // 1. 需要时规划一次（没有路线、还没到、没判死）。
            if let Some(goal) = agent.goal {
                if agent.path.is_empty() && !agent.arrived && !agent.unreachable {
                    match self.mesh.find_path([x, y], goal) {
                        Some(p) => {
                            agent.path = p;
                            agent.cursor = 1;
                            agent.repaths = agent.repaths.saturating_add(1);
                        }
                        None => agent.unreachable = true,
                    }
                }
            }

            // 2. 沿路线推进，本帧的位移预算 = speed * dt，可跨多个路点。
            if !agent.arrived && agent.cursor < agent.path.len() {
                let mut budget = agent.speed.max(0.0) * dt;
                while budget > 0.0 && agent.cursor < agent.path.len() {
                    let wp = agent.path[agent.cursor];
                    let d = dist([x, y], wp);
                    if d <= budget {
                        x = wp[0];
                        y = wp[1];
                        budget -= d;
                        agent.cursor += 1;
                    } else if d > EPS {
                        let k = budget / d;
                        x += (wp[0] - x) * k;
                        y += (wp[1] - y) * k;
                        budget = 0.0;
                    } else {
                        // 路点就在脚下（数值上重合）：跳过，不消耗预算。
                        agent.cursor += 1;
                    }
                }
                if !agent.path.is_empty() && agent.cursor >= agent.path.len() {
                    agent.arrived = true;
                    agent.path.clear();
                    agent.cursor = 0;
                }
            }

            // 3. 写回：位置、本帧实际速度（位移折算）、代理状态。
            let vx = (x - pos.x) / dt;
            let vy = (y - pos.y) / dt;
            if let Some(p) = world.get_mut::<Position>(e) {
                p.x = x;
                p.y = y;
            }
            if let Some(v) = world.get_mut::<Velocity>(e) {
                v.vx = vx;
                v.vy = vy;
            }
            if let Some(a) = world.get_mut::<NavAgent>(e) {
                *a = agent;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oriera_common::{FrameIndex, Grants, Timestamp};

    /// 三段 L 形：竖走廊（x 0..20, y 0..40）+ 拐角（y 40..60）+ 横走廊（x 20..80, y 40..60）。
    /// 内角在 (20, 40)。
    fn l_mesh() -> NavMesh {
        NavMesh::from_polygons(&[
            vec![[0.0, 0.0], [20.0, 0.0], [20.0, 40.0], [0.0, 40.0]],
            vec![[0.0, 40.0], [20.0, 40.0], [20.0, 60.0], [0.0, 60.0]],
            vec![[20.0, 40.0], [80.0, 40.0], [80.0, 60.0], [20.0, 60.0]],
        ])
        .expect("L 形网格必须建得起来")
    }

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<[f32; 2]> {
        vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]
    }

    fn ctx(dt: f32) -> FrameCtx<'static> {
        FrameCtx {
            frame: FrameIndex(0),
            dt,
            now: Timestamp(0),
            grants: Grants::EMPTY,
        }
    }

    fn close(a: [f32; 2], b: [f32; 2]) -> bool {
        dist(a, b) <= 1e-3
    }

    #[test]
    fn adjacency_comes_from_shared_edges() {
        let m = l_mesh();
        assert_eq!(m.poly_count(), 3);
        let n0: Vec<usize> = m.neighbors(0).iter().map(|n| n.poly).collect();
        let n1: Vec<usize> = m.neighbors(1).iter().map(|n| n.poly).collect();
        let n2: Vec<usize> = m.neighbors(2).iter().map(|n| n.poly).collect();
        assert_eq!(n0, vec![1], "竖走廊只接拐角");
        assert_eq!(n1, vec![0, 2], "拐角两头都接");
        assert_eq!(n2, vec![1], "横走廊只接拐角");
    }

    #[test]
    fn isolated_polygon_has_no_neighbors() {
        let m =
            NavMesh::from_polygons(&[rect(0.0, 0.0, 10.0, 10.0), rect(100.0, 0.0, 110.0, 10.0)])
                .unwrap();
        assert!(m.neighbors(0).is_empty());
        assert!(m.neighbors(1).is_empty());
    }

    #[test]
    fn straight_line_stays_two_points() {
        let m = NavMesh::from_polygons(&[rect(0.0, 0.0, 100.0, 100.0)]).unwrap();
        let p = m
            .find_path([10.0, 10.0], [90.0, 90.0])
            .expect("同一矩形内可达");
        assert_eq!(p.len(), 2, "凸多边形内不该有中间拐点: {p:?}");
        assert!(close(p[0], [10.0, 10.0]));
        assert!(close(p[1], [90.0, 90.0]));
    }

    #[test]
    fn l_corridor_turns_at_the_inner_corner() {
        let m = l_mesh();
        let p = m.find_path([10.0, 10.0], [70.0, 50.0]).expect("L 形可达");
        assert_eq!(p.len(), 3, "必须贴着内角拐一次: {p:?}");
        assert!(
            close(p[1], [20.0, 40.0]),
            "拐点应是内角 (20,40): {:?}",
            p[1]
        );
    }

    #[test]
    fn every_leg_of_the_path_is_walkable() {
        let m = l_mesh();
        let p = m.find_path([10.0, 10.0], [70.0, 50.0]).unwrap();
        for w in p.windows(2) {
            assert!(
                m.segment_walkable(w[0], w[1]),
                "这段穿出了网格: {:?} → {:?}",
                w[0],
                w[1]
            );
        }
        // 反证：不拐的直线会被判穿墙，说明校验不是恒真。
        assert!(!m.segment_walkable([10.0, 10.0], [70.0, 50.0]));
    }

    #[test]
    fn disconnected_goal_returns_none() {
        let mut polys = vec![rect(0.0, 0.0, 20.0, 20.0)];
        polys.push(rect(200.0, 0.0, 240.0, 40.0));
        let m = NavMesh::from_polygons(&polys).unwrap();
        assert!(m.find_path([10.0, 10.0], [220.0, 20.0]).is_none());
    }

    #[test]
    fn off_mesh_points_project_onto_the_mesh() {
        let m = l_mesh();
        let p = m
            .find_path([-5.0, -5.0], [70.0, 50.0])
            .expect("投影后应可达");
        assert!(close(p[0], [0.0, 0.0]), "起点应被贴到最近角点: {:?}", p[0]);
    }

    #[test]
    fn planning_is_bit_identical_across_runs() {
        let a = l_mesh().find_path([10.0, 10.0], [70.0, 50.0]).unwrap();
        let b = l_mesh().find_path([10.0, 10.0], [70.0, 50.0]).unwrap();
        assert_eq!(a, b);
        assert_eq!(path_fingerprint(&a), path_fingerprint(&b));
        // 指纹对坐标敏感：挪 1e-6 就该变。
        let mut c = a.clone();
        c[1][0] += 0.01;
        assert_ne!(path_fingerprint(&a), path_fingerprint(&c));
    }

    #[test]
    fn winding_is_normalized_and_bad_input_is_rejected() {
        // 顺时针给的矩形会被翻成正面积方向，点在内部仍判得出来。
        let cw = vec![[0.0, 0.0], [0.0, 10.0], [10.0, 10.0], [10.0, 0.0]];
        let m = NavMesh::from_polygons(&[cw]).unwrap();
        assert!(signed_double_area(m.poly(0).verts()) > 0.0);
        assert_eq!(m.locate([5.0, 5.0]), Some(0));

        assert_eq!(
            NavMesh::from_polygons(&[vec![[0.0, 0.0], [1.0, 0.0]]]),
            Err(NavError::TooFewVerts(0))
        );
        assert_eq!(
            NavMesh::from_polygons(&[vec![[0.0, 0.0], [5.0, 0.0], [10.0, 0.0]]]),
            Err(NavError::Degenerate(0))
        );
        assert_eq!(
            NavMesh::from_polygons(&[vec![
                [0.0, 0.0],
                [40.0, 0.0],
                [40.0, 40.0],
                [20.0, 20.0],
                [0.0, 40.0]
            ]]),
            Err(NavError::NonConvex(0))
        );
        assert_eq!(NavMesh::from_polygons(&[]), Err(NavError::NoPolygons));
    }

    #[test]
    fn agent_walks_the_route_and_stops_at_the_goal() {
        let mesh = l_mesh();
        let mut world = World::new();
        let e = world.spawn();
        world.insert(e, Position { x: 10.0, y: 10.0 });
        world.insert(e, Velocity { vx: 0.0, vy: 0.0 });
        let mut want = NavAgent::new(50.0);
        want.go_to([70.0, 50.0]);
        world.insert(e, want);

        let mut sys = NavAgentSystem::new(mesh.clone());
        let dt = 0.1;
        let mut frames = 0;
        while frames < 1000 {
            sys.run(&ctx(dt), &mut world);
            frames += 1;
            if world.get::<NavAgent>(e).unwrap().arrived {
                break;
            }
        }
        let a = world.get::<NavAgent>(e).unwrap();
        let p = world.get::<Position>(e).unwrap();
        assert!(a.arrived, "没走到: {frames} 帧，位置 {:?}", (p.x, p.y));
        assert!(close([p.x, p.y], [70.0, 50.0]), "终点:{:?}", (p.x, p.y));
        let p_arrived = (p.x, p.y);
        // 到达那一帧仍有部分位移（预算没走完），速度非零；下一帧才该归零。
        sys.run(&ctx(dt), &mut world);
        let p2 = world.get::<Position>(e).unwrap();
        let v = world.get::<Velocity>(e).unwrap();
        assert_eq!((p2.x, p2.y), p_arrived, "到达后不该再挪");
        assert_eq!((v.vx, v.vy), (0.0, 0.0), "到达后速度归零");
        let a = world.get::<NavAgent>(e).unwrap();
        assert!(a.path.is_empty(), "到达后路线清空");
        assert_eq!(a.repaths, 1, "只规划了一次");

        // 计划里长度约 31.6 + 51.0 = 82.6，速度 50 → 约 17 帧（允许 ±1 帧取整）。
        assert!((16..=18).contains(&frames), "帧数与路程不符: {frames}");
    }

    #[test]
    fn velocity_reports_the_actual_displacement() {
        let mesh = NavMesh::from_polygons(&[rect(0.0, 0.0, 100.0, 100.0)]).unwrap();
        let mut world = World::new();
        let e = world.spawn();
        world.insert(e, Position { x: 10.0, y: 10.0 });
        world.insert(e, Velocity { vx: 0.0, vy: 0.0 });
        let mut a = NavAgent::new(20.0);
        a.go_to([90.0, 10.0]);
        world.insert(e, a);

        let mut sys = NavAgentSystem::new(mesh);
        let dt = 0.05;
        sys.run(&ctx(dt), &mut world);
        let p = world.get::<Position>(e).unwrap();
        let v = world.get::<Velocity>(e).unwrap();
        let dx = p.x - 10.0;
        assert!(
            (dx - v.vx * dt).abs() < 1e-4,
            "位移 {dx} 与速度 {} 不符",
            v.vx
        );
        assert!((dx - 1.0).abs() < 1e-4, "一帧应走 speed*dt=1: {dx}");
        assert_eq!(v.vy, 0.0);
    }

    #[test]
    fn new_goal_mid_walk_replans_and_arrives() {
        let mesh = l_mesh();
        let mut world = World::new();
        let e = world.spawn();
        world.insert(e, Position { x: 10.0, y: 10.0 });
        world.insert(e, Velocity { vx: 0.0, vy: 0.0 });
        let mut a = NavAgent::new(50.0);
        a.go_to([70.0, 50.0]);
        world.insert(e, a);

        let mut sys = NavAgentSystem::new(mesh);
        for _ in 0..3 {
            sys.run(&ctx(0.1), &mut world);
        }
        {
            let mut ag = world.get::<NavAgent>(e).unwrap().clone();
            ag.go_to([70.0, 42.0]);
            *world.get_mut::<NavAgent>(e).unwrap() = ag;
        }
        let mut frames = 0;
        while frames < 1000 && !world.get::<NavAgent>(e).unwrap().arrived {
            sys.run(&ctx(0.1), &mut world);
            frames += 1;
        }
        let ag = world.get::<NavAgent>(e).unwrap();
        let p = world.get::<Position>(e).unwrap();
        assert!(ag.arrived);
        assert_eq!(ag.repaths, 2, "换目标应重新规划");
        assert!(close([p.x, p.y], [70.0, 42.0]), "新终点:{:?}", (p.x, p.y));
    }

    #[test]
    fn frozen_time_scale_keeps_agents_still() {
        let mesh = NavMesh::from_polygons(&[rect(0.0, 0.0, 100.0, 100.0)]).unwrap();
        let mut world = World::new();
        world.insert_resource(TimeScale(0.0));
        let e = world.spawn();
        world.insert(e, Position { x: 10.0, y: 10.0 });
        world.insert(e, Velocity { vx: 0.0, vy: 0.0 });
        let mut a = NavAgent::new(20.0);
        a.go_to([90.0, 10.0]);
        world.insert(e, a);

        let mut sys = NavAgentSystem::new(mesh);
        for _ in 0..10 {
            sys.run(&ctx(0.1), &mut world);
        }
        let p = world.get::<Position>(e).unwrap();
        assert_eq!((p.x, p.y), (10.0, 10.0), "时间冻住时 NPC 不动");
    }

    #[test]
    fn unreachable_goal_is_flagged_and_not_retried() {
        let mut polys = vec![rect(0.0, 0.0, 20.0, 20.0)];
        polys.push(rect(200.0, 0.0, 240.0, 40.0));
        let mesh = NavMesh::from_polygons(&polys).unwrap();
        let mut world = World::new();
        let e = world.spawn();
        world.insert(e, Position { x: 10.0, y: 10.0 });
        world.insert(e, Velocity { vx: 0.0, vy: 0.0 });
        let mut a = NavAgent::new(20.0);
        a.go_to([220.0, 20.0]);
        world.insert(e, a);

        let mut sys = NavAgentSystem::new(mesh);
        for _ in 0..10 {
            sys.run(&ctx(0.1), &mut world);
        }
        let ag = world.get::<NavAgent>(e).unwrap();
        assert!(ag.unreachable);
        assert!(!ag.arrived);
        assert_eq!(ag.repaths, 0, "不可达不该每帧重算");
    }
}
