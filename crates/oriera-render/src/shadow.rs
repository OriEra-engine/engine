//! L5 阴影贴图的 CPU 帧数学(feature `shadow`,默认关)——真实场景批次 WP1。
//!
//! 方向光 shadow map 的"帧数学"层:把光方向与场景包围球拟合成一台正交的
//! "光相机"([`ShadowCamera`]),把深度图容器与比较语义([`DepthMap`] /
//! [`is_lit`] / [`DEPTH_BIAS`])定义清楚。**纯 CPU、纯数学,不做 GPU pass**:
//! 真实深度图仍由渲染侧写入,本层只提供矩阵、比较规则与 CPU 对拍用的深度
//! 容器,headless 可测。
//!
//! 契约
//! - 矩阵一律 [`crate::camera::Mat4`](行主序、列向量);视空间 y 向上、
//!   镜头看向 -z;NDC z ∈ [0,1](WebGPU),与 [`crate::camera`] 同一套约定。
//!   正交投影由本模块自实现,推导写在 [`ortho_proj`] 的文档里。
//! - `light_dir` 是**光的传播方向**(世界空间,光子沿它从光源侧飞向场景):
//!   光相机的视空间 -z 轴对齐 `light_dir`,眼位在焦点上游(沿 `-light_dir`
//!   退),深度沿光传播单调增。
//! - 深度一律是**归一化深度**(投影后 [0,1],1.0 = 最远);世界偏置 =
//!   [`DEPTH_BIAS`] × (far − near)——正交深度线性,偏置在世界中均匀。
//! - [`DepthMap`] 的 uv 域是 [0,1)²、左上原点、y 向下;采样写死为最近邻。
//! - 不 `use` 同批次其它模块(`ibl` / `cull` / `instancing` / `sky` /
//!   `scene`);输入词汇只有 `(球心, 半径)` 元组与光方向。
//! - 确定性:无随机、无哈希迭代序依赖,同输入两次跑逐位一致(有测试)。
//! - 非法输入不 panic,行为逐项写死(见各条目文档);总体策略 fail-open——
//!   坏数据倾向判"亮"而不是"黑"。
//!
//! 诚实边界
//! - 只有常数深度偏置:无斜率缩放、无 PCF/软阴影、无方差/指数化深度;
//!   自阴影 acne 压不住时只能调 [`DEPTH_BIAS`],或等后续版本。
//! - 拟合是单一包围球:多球先归并成一颗包围球([`enclosing_sphere`],保守
//!   偏松),不做级联(CSM)、不做纹素稳定化(光旋转时影缘闪烁是已知代价)。
//! - 最近邻采样有硬边锯齿;本层比较规则与 GPU 可比较采样器不逐位对齐,
//!   用于 CPU 验证与回读对拍,不替代 GPU pass。
//! - 球心分量绝对值超过 ~1e17 时,包围球半径可能溢出为 inf:交由
//!   [`ShadowCamera::from_light_dir`] 兜底,结果无意义但仍有限、不 panic;
//!   现实场景尺度远达不到。

use crate::camera::{mul, transform, view_look_at, Mat4};

/// 阴影比较的深度偏置,**归一化深度单位**(投影后 [0,1] 域上的绝对量)。
///
/// 判定规则见 [`is_lit`]:片元深度 ≤ 存储深度 + 偏置 → lit。重合、更近、
/// 以及落在偏置带内都算亮,超出偏置带才算阴影。世界空间偏置 =
/// `DEPTH_BIAS × (far − near)`(正交投影深度线性,偏置在世界中均匀)。
///
/// 0.002 是经验起点:对几十米量级的拟合盒约为厘米级,足以压住多数自阴影
/// acne;按场景调整,斜率缩放不在本层(见模块"诚实边界")。
pub const DEPTH_BIAS: f32 = 0.002;

/// 拟合半径的兜底下限:`radius` ≤ 0 或非有限时按此值拟合。只保证投影矩阵
/// 有限、不 panic,不保证拟合有意义——调用方应传正的、现实的半径。
const MIN_FIT_RADIUS: f32 = 1e-3;

/// 方向光影子相机:一台包住焦点球的正交相机,深度沿光传播单调增。
///
/// 几何与单位见模块文档;构造入口 [`ShadowCamera::from_light_dir`] /
/// [`ShadowCamera::from_spheres`]。矩阵可直接喂 GPU 侧(行主序、列向量,
/// 上传时按引擎惯例转置),CPU 侧用 [`ShadowCamera::project`] 对拍。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShadowCamera {
    /// 世界 → 光视空间。视空间 -z = 光传播方向,y 向上。
    pub view: Mat4,
    /// 光视空间 → 裁剪空间。正交;NDC x/y ∈ [-1,1]、z ∈ [0,1](WebGPU)。
    pub proj: Mat4,
}

impl ShadowCamera {
    /// 由光方向 + 焦点球拟合影子相机。
    ///
    /// 几何:`light_dir` 是光的传播方向;眼位于 `focus` 上游——沿
    /// `-light_dir` 退 `radius`(贴住球面),视线沿 `light_dir` 看向 focus。
    /// 于是视空间 **-z 轴 = `light_dir`**(把 `light_dir` 过 view 的旋转
    /// 部分应得 `[0, 0, -1]`),深度随光传播单调增。
    ///
    /// 拟合盒:[`ortho_fit`] 的盒再加 pad——`near_pad` 把近面向光侧外扩
    /// (容纳焦点球与光之间的遮挡体;近面可退到眼后,即 near < 0,正交
    /// 投影下合法),`far_pad` 把远面沿光传播外扩(容纳焦点球背后的接收体)。
    ///
    /// up 固定 `[0, 1, 0]`(y 向上世界);光与 up 共线(顶光/仰光)时由
    /// [`view_look_at`] 的共线急救换参考轴,结果确定。
    ///
    /// 退化输入不 panic、有定义(fail-open):
    /// - `light_dir` 非有限、零向量或长度 < 1e-6 → 按世界顶光 `[0, -1, 0]`;
    /// - `focus` 的非有限分量 → 按 `0.0`;
    /// - `radius` ≤ 0 或非有限 → 按 [`MIN_FIT_RADIUS`];
    /// - `near_pad` / `far_pad` 负或非有限 → 按 `0.0`。
    pub fn from_light_dir(
        light_dir: [f32; 3],
        focus: [f32; 3],
        radius: f32,
        near_pad: f32,
        far_pad: f32,
    ) -> Self {
        // 光方向消毒:先查分量有限与最大分量,规避 NaN 传染与平方溢出。
        let max_abs = light_dir.iter().fold(0.0_f32, |m, v| m.max(v.abs()));
        let dir = if light_dir.iter().all(|v| v.is_finite()) && max_abs > 1e-6 {
            let s = 1.0 / max_abs;
            // 按最大分量缩放后再求长度:n ∈ [1, √3],全程有限。
            let n = ((light_dir[0] * s).powi(2)
                + (light_dir[1] * s).powi(2)
                + (light_dir[2] * s).powi(2))
            .sqrt();
            [
                light_dir[0] * s / n,
                light_dir[1] * s / n,
                light_dir[2] * s / n,
            ]
        } else {
            [0.0, -1.0, 0.0]
        };
        let fix = |v: f32| if v.is_finite() { v } else { 0.0 };
        let focus = [fix(focus[0]), fix(focus[1]), fix(focus[2])];
        let radius = if radius.is_finite() && radius > 0.0 {
            radius
        } else {
            MIN_FIT_RADIUS
        };
        let near_pad = if near_pad.is_finite() {
            near_pad.max(0.0)
        } else {
            0.0
        };
        let far_pad = if far_pad.is_finite() {
            far_pad.max(0.0)
        } else {
            0.0
        };
        // 眼在上游:沿 -light_dir 退 radius,贴住焦点球。
        let eye = [
            focus[0] - dir[0] * radius,
            focus[1] - dir[1] * radius,
            focus[2] - dir[2] * radius,
        ];
        let view = view_look_at(eye, focus, [0.0, 1.0, 0.0]);
        // 拟合盒 + pad:near/far 是从眼出发的深度距离(可为负 = 平面在眼后)。
        let (half_width, near, far) = ortho_fit(focus, radius);
        let proj = ortho_proj(half_width, near - near_pad, far + far_pad);
        Self { view, proj }
    }

    /// 多球入口:世界空间球列表(球心 + 半径)+ 光方向 → 包住所有球的
    /// 影子相机。先 [`enclosing_sphere`] 归并,再 [`ShadowCamera::from_light_dir`]。
    ///
    /// 空表 / 全垃圾球 → 零球 → 按 [`MIN_FIT_RADIUS`] 兜底:矩阵有限但拟合
    /// 无意义,调用方应保证至少一颗合法球。
    pub fn from_spheres(
        light_dir: [f32; 3],
        spheres: &[([f32; 3], f32)],
        near_pad: f32,
        far_pad: f32,
    ) -> Self {
        let (focus, radius) = enclosing_sphere(spheres);
        Self::from_light_dir(light_dir, focus, radius, near_pad, far_pad)
    }

    /// `proj * view`(一次乘好;shader 与 CPU 对拍只吃这一个矩阵)。
    pub fn view_proj(&self) -> Mat4 {
        mul(&self.proj, &self.view)
    }

    /// 世界点 → 影图采样坐标 `[u, v, depth]`。
    ///
    /// u/v ∈ [0,1](左上原点、y 向下,与 [`DepthMap`] 的采样约定一致),
    /// depth ∈ [0,1] 归一化深度(可直接喂 [`is_lit`])。点不在拟合盒内
    /// ——NDC 出 `[−1,1]² × [0,1]`、w ≤ 0、或结果非有限——返回 `None`,
    /// 调用方按盒外 = lit 处理,与 [`is_lit`] 的出界语义一致。
    pub fn project(&self, p: [f32; 3]) -> Option<[f32; 3]> {
        let c = transform(&self.view_proj(), p);
        if c[3] <= 0.0 || !c.iter().all(|v| v.is_finite()) {
            return None;
        }
        let ndc = [c[0] / c[3], c[1] / c[3], c[2] / c[3]];
        if !(-1.0..=1.0).contains(&ndc[0])
            || !(-1.0..=1.0).contains(&ndc[1])
            || !(0.0..=1.0).contains(&ndc[2])
        {
            return None;
        }
        // NDC(y 向上)→ uv(左上原点、y 向下)。
        Some([(ndc[0] + 1.0) * 0.5, (1.0 - ndc[1]) * 0.5, ndc[2]])
    }
}

/// "这颗球可参与拟合":中心有限、半径有限且为正。
fn plausible(c: [f32; 3], r: f32) -> bool {
    c.iter().all(|v| v.is_finite()) && r.is_finite() && r > 0.0
}

/// 一组球的包围球(纯函数):世界空间 `(球心, 半径)` 列表 → `(球心, 半径)`。
///
/// 做法:各球 `球心 ± 半径` 的 AABB 并集中心作球心,半径取
/// `max_i(|c_i − center| + r_i)`——每颗球都被严格包住(球面上任一点到
/// center 的距离 ≤ |c_i − center| + r_i)。顺序无关、确定;单球输入精确
/// 还原;双球沿连线时即最小包围球;一般情形保守偏松(不做迭代收紧)。
///
/// 非有限中心、或半径 ≤ 0 / 非有限的球被跳过,不参与拟合;空表 / 全垃圾
/// → 零球 `((0,0,0), 0.0)`,由 [`ShadowCamera::from_light_dir`] 兜底。
pub fn enclosing_sphere(spheres: &[([f32; 3], f32)]) -> ([f32; 3], f32) {
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    let mut any = false;
    for &(c, r) in spheres {
        if !plausible(c, r) {
            continue; // 垃圾球跳过,不毒化拟合。
        }
        any = true;
        for a in 0..3 {
            lo[a] = lo[a].min(c[a] - r);
            hi[a] = hi[a].max(c[a] + r);
        }
    }
    if !any {
        return ([0.0; 3], 0.0);
    }
    // 各乘 0.5 再相加,避免 lo + hi 在极端分量下先溢出。
    let center = [
        lo[0] * 0.5 + hi[0] * 0.5,
        lo[1] * 0.5 + hi[1] * 0.5,
        lo[2] * 0.5 + hi[2] * 0.5,
    ];
    let mut radius = 0.0_f32;
    for &(c, r) in spheres {
        if !plausible(c, r) {
            continue;
        }
        let d0 = c[0] - center[0];
        let d1 = c[1] - center[1];
        let d2 = c[2] - center[2];
        radius = radius.max((d0 * d0 + d1 * d1 + d2 * d2).sqrt() + r);
    }
    (center, radius)
}

/// 正交拟合盒(纯函数):世界空间球 → `(half_width, near, far)`(半宽/近/远)。
///
/// - `half_width = radius`:球在垂直于光轴的平面上的投影是同半径圆,盒的
///   x/y 半宽取它即可包住球;
/// - `near` / `far` 是**从眼出发的深度距离**,配套眼位约定:眼在球心上游、
///   沿光轴退 `radius`(贴住球面)——球恰好占据深度 `[0, 2·radius]`,故
///   `near = 0`、`far = 2·radius`;[`ShadowCamera::from_light_dir`] 在此之上
///   施加 pad(`near − near_pad`、`far + far_pad`)。
/// - `center` 不参与数值:拟合只依赖半径与上述眼位约定,签名保留它是为了
///   调用点语义完整——拟合的对象就是这个球;传任意球心,结果相同。
///
/// `radius` ≤ 0 或非有限时按 [`MIN_FIT_RADIUS`] 兜底(矩阵有限,不出 NaN)。
pub fn ortho_fit(center: [f32; 3], radius: f32) -> (f32, f32, f32) {
    let _ = center; // 见文档:center 不参与数值。
    let r = if radius.is_finite() && radius > 0.0 {
        radius
    } else {
        MIN_FIT_RADIUS
    };
    (r, 0.0, 2.0 * r)
}

/// 正交投影矩阵(视空间 → 裁剪空间;自实现,推导在此文档化)。
///
/// 与 [`crate::camera::perspective`] 同一套约定:列向量、行主序;视空间
/// y 向上、镜头看向 -z(眼前 z 为负);**NDC z ∈ [0,1]**(WebGPU);正交
/// 无透视除法,w = 1。映射:
///
/// - x:`[-half_width, half_width] → [-1, 1]`(y 对称同款);
/// - z:记深度 `d = −z_view`(从眼出发的距离),`[near, far] → [0, 1]`,
///   即第三行 `[0, 0, 1/(near−far), near/(near−far)]`——与 perspective 的
///   第三行同款分母,代入 `z_view = −near → 0`、`z_view = −far → 1` 可验。
///
/// 要求 `half_width > 0` 且 `far > near`(near 可为负:平面退到眼后,正交
/// 投影合法);[`ShadowCamera`] 的构造链已消毒保证。
fn ortho_proj(half_width: f32, near: f32, far: f32) -> Mat4 {
    let inv = 1.0 / half_width;
    let denom = near - far;
    [
        [inv, 0.0, 0.0, 0.0],
        [0.0, inv, 0.0, 0.0],
        [0.0, 0.0, 1.0 / denom, near / denom],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// CPU 侧深度图:`w×h` 的归一化深度网格([0,1],**1.0 = 最远**)。
///
/// 存储行主、左上原点、y 向下(与 [`ShadowCamera::project`] 的 uv 同一套):
/// `data[y·w + x]`。契约:
///
/// - [`DepthMap::new`] 以 1.0(最远)清屏——"没写入过的区域 = lit";
/// - [`DepthMap::set`] 把深度规范到 [0,1](越界夹取、非有限记 1.0):图的
///   不变量是"全格有限且在 [0,1]",[`is_lit`] 不必再防图侧坏值;
/// - [`DepthMap::get`] 出界返回 1.0(最远 = lit),不 panic;
/// - 采样写死为**最近邻**([`DepthMap::sample`]):uv ∈ [0,1)² 映到
///   `floor(uv·size)` 号纹素;uv = 1.0、出界、非有限按最远(1.0)计;
///   硬边锯齿是预期行为(见模块"诚实边界")。
/// - `w` 或 `h` 为 0 的空图合法:一切采样都出界。分配上限不做——纹理
///   尺寸是调用方的资源决定。
#[derive(Debug, Clone, PartialEq)]
pub struct DepthMap {
    pub w: u32,
    pub h: u32,
    data: Vec<f32>,
}

impl DepthMap {
    /// `w×h` 深度图,全部清成 1.0(最远)。
    pub fn new(w: u32, h: u32) -> Self {
        Self {
            w,
            h,
            data: vec![1.0; w as usize * h as usize],
        }
    }

    /// 写纹素 `(x, y)`:出界为无操作;深度规范到 [0,1](非有限记 1.0)。
    pub fn set(&mut self, x: u32, y: u32, depth: f32) {
        if x >= self.w || y >= self.h {
            return;
        }
        let d = if depth.is_finite() {
            depth.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self.data[y as usize * self.w as usize + x as usize] = d;
    }

    /// 读纹素 `(x, y)`:出界返回 1.0(最远,即 lit),不 panic。
    pub fn get(&self, x: u32, y: u32) -> f32 {
        if x >= self.w || y >= self.h {
            return 1.0;
        }
        self.data[y as usize * self.w as usize + x as usize]
    }

    /// 最近邻采样:uv → `floor(uv·size)` 号纹素值;越界 / 非有限 / 空图
    /// 返回 1.0(最远)。
    pub fn sample(&self, uv: [f32; 2]) -> f32 {
        match self.texel(uv) {
            Some((x, y)) => self.get(x, y),
            None => 1.0,
        }
    }

    /// uv → 最近纹素下标;越界(域 [0,1)² 之外)/ 非有限 / 空图 → `None`。
    fn texel(&self, uv: [f32; 2]) -> Option<(u32, u32)> {
        let (u, v) = (uv[0], uv[1]);
        if self.w == 0 || self.h == 0 || !u.is_finite() || !v.is_finite() {
            return None;
        }
        if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
            return None; // uv = 1.0 落在第 w(h)号纹素,按出界计。
        }
        // floor 后夹取:防 (1−ε)·w 舍入到 w 的极端情况。
        let x = ((u * self.w as f32) as u32).min(self.w - 1);
        let y = ((v * self.h as f32) as u32).min(self.h - 1);
        Some((x, y))
    }
}

/// 阴影判定:片元在 `uv` 处是否被照亮。
///
/// 取 `uv` 的最近纹素深度 `d`(采样约定见 [`DepthMap`]),比较:
///
/// - `frag_depth ≤ d + DEPTH_BIAS` → lit:重合、更近、偏置带内都算亮
///   (偏置吸收浮点与纹素量化误差,防止自阴影 acne);
/// - `frag_depth > d + DEPTH_BIAS` → 阴影:片元明显深于已记录的最近面,
///   中间有遮挡物。
///
/// 出界与非有限语义(fail-open,全部写死、不 panic):
/// - uv 越界(域 [0,1)² 之外)或非有限 → lit(片元不在影图覆盖内);
/// - `frag_depth` 非有限 → lit(坏数据亮而非黑)。
///
/// 图侧坏值无需再防:[`DepthMap::set`] 已保证格值有限且在 [0,1]。
pub fn is_lit(map: &DepthMap, uv: [f32; 2], frag_depth: f32) -> bool {
    if !frag_depth.is_finite() {
        return true;
    }
    match map.texel(uv) {
        None => true,
        Some((x, y)) => frag_depth <= map.get(x, y) + DEPTH_BIAS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 相对容差(旋转引入的舍入;轴对齐路径才用逐位精确断言)。
    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4 * b.abs().max(1.0)
    }

    fn finite(m: &Mat4) -> bool {
        m.iter().all(|row| row.iter().all(|v| v.is_finite()))
    }

    /// 斜 45° 光:沿世界 x−y 对角向下射(+x、−y 方向)。
    fn tilted_dir() -> [f32; 3] {
        let s = std::f32::consts::FRAC_1_SQRT_2;
        [s, -s, 0.0]
    }

    /// 视图矩阵的旋转部分作用在方向上(平移不作用于方向)。
    fn view_dir(view: &Mat4, d: [f32; 3]) -> [f32; 3] {
        [
            view[0][0] * d[0] + view[0][1] * d[1] + view[0][2] * d[2],
            view[1][0] * d[0] + view[1][1] * d[1] + view[1][2] * d[2],
            view[2][0] * d[0] + view[2][1] * d[1] + view[2][2] * d[2],
        ]
    }

    #[test]
    fn view_aligns_light_dir_to_minus_z() {
        let dir = tilted_dir();
        let cam = ShadowCamera::from_light_dir(dir, [3.0, 2.0, -1.0], 2.0, 0.5, 1.5);
        let rd = view_dir(&cam.view, dir);
        assert!(
            close(rd[0], 0.0) && close(rd[1], 0.0) && close(rd[2], -1.0),
            "光传播方向必须对齐视空间 -z: {rd:?}"
        );
        // 焦点在光轴上,深度 = radius(眼贴球面)。
        let f = transform(&cam.view, [3.0, 2.0, -1.0]);
        assert!(
            close(f[0], 0.0) && close(f[1], 0.0) && close(f[2], -2.0),
            "{f:?}"
        );
    }

    #[test]
    fn top_light_keeps_exact_axes_and_rescues_collinear_up() {
        // dir 与 up 共线(顶光):走 view_look_at 的急救轴,结果仍确定、精确。
        let cam = ShadowCamera::from_light_dir([0.0, -1.0, 0.0], [3.0, 2.0, 5.0], 2.0, 0.0, 0.0);
        assert_eq!(view_dir(&cam.view, [0.0, -1.0, 0.0]), [0.0, 0.0, -1.0]);
        assert_eq!(transform(&cam.view, [3.0, 2.0, 5.0]), [0.0, 0.0, -2.0, 1.0]);
        assert!(finite(&cam.view) && finite(&cam.proj));
    }

    #[test]
    fn sphere_points_stay_inside_the_fit_box() {
        let dir = tilted_dir();
        let focus = [3.0, 2.0, -1.0];
        let r = 2.0;
        let cam = ShadowCamera::from_light_dir(dir, focus, r, 0.5, 1.5);
        let c = cam.project(focus).expect("焦点必在盒内");
        assert!(close(c[0], 0.5) && close(c[1], 0.5), "焦点是盒心: {c:?}");
        assert!(c[2] > 0.0 && c[2] < 1.0);
        // 球面 8 个方向(留 0.1% 余量避开旋转舍入的边界抖动)。
        let s = (1.0f32 / 3.0).sqrt(); // 1/√3:对角方向每分量取值
        let dirs = [
            [1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, -1.0],
            [s, s, s],
            [-s, -s, -s],
        ];
        for a in dirs {
            let p = [
                focus[0] + a[0] * r * 0.999,
                focus[1] + a[1] * r * 0.999,
                focus[2] + a[2] * r * 0.999,
            ];
            let uv = cam.project(p).unwrap_or_else(|| panic!("{a:?} 逃出拟合盒"));
            assert!(
                (0.0..=1.0).contains(&uv[0]) && (0.0..=1.0).contains(&uv[1]),
                "{a:?} → {uv:?}"
            );
            assert!((0.0..=1.0).contains(&uv[2]), "深度出域 {a:?} → {uv:?}");
        }
    }

    #[test]
    fn points_beyond_the_sphere_leave_the_fit_box() {
        let dir = tilted_dir();
        let focus = [3.0, 2.0, -1.0];
        let cam = ShadowCamera::from_light_dir(dir, focus, 2.0, 0.5, 1.5);
        let along = |t: f32| {
            [
                focus[0] + dir[0] * t,
                focus[1] + dir[1] * t,
                focus[2] + dir[2] * t,
            ]
        };
        // 深度向出盒:远面之外(沿光传播 6,far 距离 5.5)。
        assert!(cam.project(along(6.0)).is_none(), "超出 far 仍算盒内?");
        // 近面之外(眼后,逆光 3;near 距离 −0.5)。
        assert!(cam.project(along(-3.0)).is_none(), "退到眼后仍算盒内?");
        // 侧面出盒:沿 [1,1,0](⊥ 光方向,深度与焦点相同)移出,纯粹 xy 出盒。
        assert!(
            cam.project([focus[0] + 4.0, focus[1] + 4.0, focus[2]])
                .is_none(),
            "侧面出盒点仍算盒内?"
        );
    }

    #[test]
    fn pads_extend_the_depth_range_both_ways() {
        let dir = [0.0, -1.0, 0.0];
        let focus = [0.0, 0.0, 0.0];
        let r = 2.0;
        let bare = ShadowCamera::from_light_dir(dir, focus, r, 0.0, 0.0);
        let padded = ShadowCamera::from_light_dir(dir, focus, r, 1.0, 1.0);
        // 无 pad:焦点球上下游端点恰落在深度边界(near 平面 0、far 平面 1)。
        let near_tip = [0.0, 2.0, 0.0]; // focus − dir·r,贴眼端
        let far_tip = [0.0, -2.0, 0.0]; // focus + dir·r,下游端
        assert!(close(bare.project(near_tip).expect("贴眼端在近面")[2], 0.0));
        assert!(close(bare.project(far_tip).expect("下游端在远面")[2], 1.0));
        // 加 pad:同一端点离开边界,球外上游/下游点被纳入。
        let p = padded.project(near_tip).expect("padded 近面外扩");
        assert!(p[2] > 0.0 && p[2] < 1.0);
        let p = padded.project(far_tip).expect("padded 远面外扩");
        assert!(p[2] > 0.0 && p[2] < 1.0);
        assert!(
            padded.project([0.0, 2.5, 0.0]).is_some(),
            "近 pad 应纳入球外上游点"
        );
        assert!(
            padded.project([0.0, -2.5, 0.0]).is_some(),
            "远 pad 应纳入球外下游点"
        );
        assert!(
            padded.project([0.0, 3.5, 0.0]).is_none(),
            "超出近 pad 仍出盒"
        );
        assert!(
            padded.project([0.0, -3.5, 0.0]).is_none(),
            "超出远 pad 仍出盒"
        );
    }

    #[test]
    fn box_edges_are_inclusive_on_exact_axes() {
        // 顶光 + 焦点在原点:纯平移缩放数学,边界断言可精确到逐位。
        let cam = ShadowCamera::from_light_dir([0.0, -1.0, 0.0], [0.0, 0.0, 0.0], 2.0, 0.0, 0.0);
        // 球的四个侧端点恰在盒缘(u/v = 0 或 1),深度 0.5。
        assert_eq!(cam.project([2.0, 0.0, 0.0]), Some([0.5, 0.0, 0.5]));
        assert_eq!(cam.project([-2.0, 0.0, 0.0]), Some([0.5, 1.0, 0.5]));
        assert_eq!(cam.project([0.0, 0.0, 2.0]), Some([1.0, 0.5, 0.5]));
        assert_eq!(cam.project([0.0, 0.0, -2.0]), Some([0.0, 0.5, 0.5]));
        // 深度端点:贴眼端 0、下游端 1(无 pad)。
        assert_eq!(cam.project([0.0, 2.0, 0.0]), Some([0.5, 0.5, 0.0]));
        assert_eq!(cam.project([0.0, -2.0, 0.0]), Some([0.5, 0.5, 1.0]));
        // 越过边界一步即出盒。
        assert_eq!(cam.project([3.0, 0.0, 0.0]), None);
        assert_eq!(cam.project([0.0, 0.0, 3.0]), None);
        assert_eq!(cam.project([0.0, 3.0, 0.0]), None);
        assert_eq!(cam.project([0.0, -3.0, 0.0]), None);
    }

    #[test]
    fn ortho_fit_is_pure_and_center_free() {
        assert_eq!(ortho_fit([0.0; 3], 2.5), (2.5, 0.0, 5.0));
        assert_eq!(ortho_fit([7.0, -4.0, 100.0], 0.25), (0.25, 0.0, 0.5));
        // center 不参与数值(文档写死)。
        assert_eq!(ortho_fit([0.0; 3], 1.0), ortho_fit([9.0, 9.0, 9.0], 1.0));
        // 退化半径兜底。
        let min = (MIN_FIT_RADIUS, 0.0, 2.0 * MIN_FIT_RADIUS);
        assert_eq!(ortho_fit([0.0; 3], 0.0), min);
        assert_eq!(ortho_fit([0.0; 3], -3.0), min);
        assert_eq!(ortho_fit([0.0; 3], f32::NAN), min);
    }

    #[test]
    fn invalid_light_dir_falls_back_to_top_light() {
        let ok = ShadowCamera::from_light_dir([0.0, -1.0, 0.0], [1.0, 2.0, 3.0], 2.0, 0.0, 0.0);
        assert!(finite(&ok.view) && finite(&ok.proj));
        // 零向量 / NaN / inf / 次正规长度 → 一律顶光兜底,逐位一致、不 panic。
        for bad in [
            [0.0, 0.0, 0.0],
            [f32::NAN, 0.0, 0.0],
            [f32::INFINITY, 0.0, 0.0],
            [1e-8, 0.0, 0.0],
            [f32::NEG_INFINITY, 1.0, 2.0],
        ] {
            let cam = ShadowCamera::from_light_dir(bad, [1.0, 2.0, 3.0], 2.0, 0.0, 0.0);
            assert_eq!(cam, ok, "{bad:?} 必须兜底为世界顶光");
        }
    }

    #[test]
    fn degenerate_focus_radius_and_pads_stay_finite() {
        let cam = ShadowCamera::from_light_dir(
            [0.0, -1.0, 0.0],
            [f32::NAN, f32::INFINITY, 1.0],
            -4.0,
            -1.0,
            f32::NAN,
        );
        assert!(finite(&cam.view) && finite(&cam.proj));
        // 非法输入有确定值:focus 坏分量归零、radius 兜底、负/NaN pad 归零。
        let canon = ShadowCamera::from_light_dir(
            [0.0, -1.0, 0.0],
            [0.0, 0.0, 1.0],
            MIN_FIT_RADIUS,
            0.0,
            0.0,
        );
        assert_eq!(cam, canon);
    }

    #[test]
    fn enclosing_sphere_wraps_every_sphere() {
        let spheres = [
            ([1.0, 0.0, 0.0], 1.0),
            ([3.0, 4.0, 0.0], 2.0),
            ([-2.0, -2.0, 5.0], 0.5),
        ];
        let (c, r) = enclosing_sphere(&spheres);
        for (s, sr) in spheres {
            let d0 = s[0] - c[0];
            let d1 = s[1] - c[1];
            let d2 = s[2] - c[2];
            let d = (d0 * d0 + d1 * d1 + d2 * d2).sqrt();
            assert!(d + sr <= r + 1e-4, "({s:?},{sr}) 逃出包围球: d={d} r={r}");
        }
        // 顺序无关。
        let mut rev = spheres;
        rev.reverse();
        assert_eq!(enclosing_sphere(&rev), (c, r));
    }

    #[test]
    fn from_spheres_covers_all_and_skips_garbage() {
        let spheres = [([0.0, 0.0, 0.0], 1.0), ([4.0, 0.0, 0.0], 1.0)];
        let cam = ShadowCamera::from_spheres([0.0, -1.0, 0.0], &spheres, 0.5, 0.5);
        for s in [
            [0.0, 0.0, 0.0],
            [4.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [4.0, -1.0, 0.0],
        ] {
            assert!(cam.project(s).is_some(), "{s:?} 未被包住");
        }
        // 垃圾球(非有限中心 / 非法半径)跳过,不毒化拟合。
        let garbled = [
            ([0.0, 0.0, 0.0], 1.0),
            ([f32::NAN; 3], 5.0),
            ([0.0, 0.0, 0.0], -1.0),
        ];
        assert_eq!(enclosing_sphere(&garbled), ([0.0, 0.0, 0.0], 1.0));
        // 空表 → 零球 → MIN 兜底:矩阵有限,不 panic。
        let empty = ShadowCamera::from_spheres([0.0, -1.0, 0.0], &[], 0.0, 0.0);
        assert!(finite(&empty.view) && finite(&empty.proj));
    }

    #[test]
    fn depth_map_set_get_clamps_and_defaults() {
        let mut m = DepthMap::new(3, 2);
        assert_eq!((m.w, m.h), (3, 2));
        // 清屏 = 1.0(最远)。
        assert_eq!(m.get(0, 0), 1.0);
        assert_eq!(m.get(2, 1), 1.0);
        // 规范化:越界夹取、非有限记 1.0。
        m.set(1, 0, 0.25);
        m.set(0, 1, 1.5);
        m.set(2, 1, -0.5);
        m.set(0, 0, f32::NAN);
        assert_eq!(m.get(1, 0), 0.25);
        assert_eq!(m.get(0, 1), 1.0);
        assert_eq!(m.get(2, 1), 0.0);
        assert_eq!(m.get(0, 0), 1.0);
        // 出界读写:不 panic,set 无操作,get 记 1.0。
        m.set(3, 0, 0.0);
        m.set(0, 2, 0.0);
        m.set(u32::MAX, u32::MAX, 0.0);
        assert_eq!(m.get(3, 0), 1.0);
        assert_eq!(m.get(0, 2), 1.0);
        assert_eq!(m.get(1, 1), 1.0, "出界 set 不得污染图");
        // 空图:一切出界。
        let empty = DepthMap::new(0, 5);
        assert_eq!(empty.get(0, 0), 1.0);
        assert_eq!(empty.sample([0.5, 0.5]), 1.0);
    }

    #[test]
    fn nearest_sampling_maps_uv_to_texels() {
        // 2×2:uv 域 [0,0.5) → 列 0,[0.5,1) → 列 1;左上原点、y 向下。
        let mut m = DepthMap::new(2, 2);
        m.set(0, 0, 0.1);
        m.set(1, 0, 0.2);
        m.set(0, 1, 0.3);
        m.set(1, 1, 0.4);
        assert_eq!(m.sample([0.0, 0.0]), 0.1, "左上角第一纹素");
        assert_eq!(m.sample([0.49, 0.0]), 0.1);
        assert_eq!(m.sample([0.5, 0.0]), 0.2, "半界归右");
        assert_eq!(m.sample([0.0, 0.5]), 0.3, "y 向下:第二行");
        assert_eq!(m.sample([0.9, 0.9]), 0.4);
        // uv = 1.0 / 出界 / NaN → 最远(1.0)。
        assert_eq!(m.sample([1.0, 0.0]), 1.0);
        assert_eq!(m.sample([-0.1, 0.5]), 1.0);
        assert_eq!(m.sample([f32::NAN, 0.5]), 1.0);
    }

    #[test]
    fn is_lit_bias_semantics() {
        let mut m = DepthMap::new(4, 4);
        for y in 0..4u32 {
            for x in 0..4u32 {
                m.set(x, y, 0.5);
            }
        }
        let uv = [0.6, 0.3];
        // 重合 → lit;更近 → lit。
        assert!(is_lit(&m, uv, 0.5), "重合深度必须 lit");
        assert!(is_lit(&m, uv, 0.49));
        // 略深于 map(偏置带内、含边界)→ lit。
        assert!(is_lit(&m, uv, 0.5 + DEPTH_BIAS * 0.5), "偏置带内算 lit");
        assert!(is_lit(&m, uv, 0.5 + DEPTH_BIAS), "偏置边界(含)算 lit");
        // 明显更深(超偏置带)→ 阴影。
        assert!(!is_lit(&m, uv, 0.5 + DEPTH_BIAS * 2.0), "超偏置带必须阴影");
        // frag 非有限 → fail-open lit。
        assert!(is_lit(&m, uv, f32::NAN));
    }

    #[test]
    fn is_lit_out_of_bounds_and_garbage_uv_is_lit() {
        let mut m = DepthMap::new(2, 2);
        for y in 0..2u32 {
            for x in 0..2u32 {
                m.set(x, y, 0.1);
            }
        }
        for bad_uv in [
            [-0.01, 0.5],
            [1.0, 0.5],
            [0.5, 1.0],
            [1.5, 1.5],
            [f32::NAN, 0.5],
            [0.5, f32::INFINITY],
        ] {
            assert!(is_lit(&m, bad_uv, 0.9), "{bad_uv:?} 出界必须 lit");
        }
        // 图外深度再近也不翻转出界语义。
        assert!(is_lit(&m, [-1.0, -1.0], 0.0));
    }

    #[test]
    fn view_proj_composes_proj_after_view() {
        let cam = ShadowCamera::from_light_dir(tilted_dir(), [1.0, 2.0, 3.0], 2.0, 0.0, 1.0);
        assert_eq!(cam.view_proj(), mul(&cam.proj, &cam.view));
        // project 与 view_proj + NDC→uv 手工换算一致(抽查焦点)。
        let c = transform(&cam.view_proj(), [1.0, 2.0, 3.0]);
        let ndc = [c[0] / c[3], c[1] / c[3], c[2] / c[3]];
        let uv = cam.project([1.0, 2.0, 3.0]).expect("焦点在盒内");
        assert!(close(uv[0], (ndc[0] + 1.0) * 0.5));
        assert!(close(uv[1], (1.0 - ndc[1]) * 0.5));
        assert!(close(uv[2], ndc[2]));
    }

    #[test]
    fn same_inputs_run_bit_identical() {
        let spheres = [
            ([1.0, 2.0, 3.0], 1.5),
            ([3.0, 1.0, 2.5], 2.5),
            ([-1.0, 0.5, 2.0], 1.0),
        ];
        let light = tilted_dir();
        let build = || {
            let cam = ShadowCamera::from_spheres(light, &spheres, 0.3, 1.2);
            let mut m = DepthMap::new(32, 32);
            for y in 0..32u32 {
                for x in 0..32u32 {
                    let u = (x as f32 + 0.5) / 32.0;
                    let v = (y as f32 + 0.5) / 32.0;
                    m.set(x, y, (u + v) * 0.5);
                }
            }
            (cam, m)
        };
        let (a, ma) = build();
        let (b, mb) = build();
        assert_eq!(a, b, "相机矩阵必须逐位一致");
        assert_eq!(ma, mb, "深度图必须逐位一致");
        for uv in [[0.1, 0.2], [0.9, 0.8], [0.5, 0.5]] {
            assert_eq!(is_lit(&ma, uv, 0.4), is_lit(&mb, uv, 0.4));
        }
    }
}
