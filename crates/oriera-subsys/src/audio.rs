//! L6 空间音频:纯几何层(feature `audio`,默认关)——真实场景批次 WP8。
//!
//! 职责:回答"听者在这里、声源在那里,每个声源该以多大增益、偏到哪一侧
//! 播出来"。只做确定性几何计算,输出给混音器的增益与声像([`SourceMix`]);
//! 音频设备、解码、混音、混响都不在引擎内——本层输出纯数据,谁拿到谁负责
//! 喂给混音器。CI 腿:`subsys-nav-anim-audio`。
//!
//! **坐标与朝向约定**(与渲染侧一致)
//! - 右手系、**y 向上**;位置一律 `[f32; 3]`,长度单位与世界单位一致(米);
//! - [`Listener::forward`] 是"看向"的方向、[`Listener::up`] 是头顶方向,
//!   **右轴 = forward × up 再归一化**(右手系叉积)。例:forward=(0,0,-1)、
//!   up=(0,1,0) 时右轴=(1,0,0);声像 +1 = 声源在右轴方向上(正右);
//! - `up` 不必与 `forward` 正交,只需不平行(见 [`Listener::new`])。
//!
//! **契约**
//! - 纯 CPU、headless 可测;纯 std、零新增依赖,wasm32 可编译;
//! - 距离衰减 `1 / (1 + falloff_k · d²)`(d 为听者-声源欧氏距离);
//!   `d > max_dist` 时增益置 0,**但仍返回完整 [`SourceMix`],不剔除声源**
//!   (声源剔除与生命周期在上层,本层不维护声源列表);
//! - 声像:声源相对听者的单位方向在右轴上的投影,clamp 到 [-1,+1];
//!   重合位置(相距 < 1e-6)→ pan 0、增益满;
//! - 指向性:[`Directivity::Cone`] 按"声源→听者方向"与锥轴的夹角(度)取值,
//!   锥内 1、锥外 `outer_gain`(使用时 clamp 到 [0,1])、之间线性;锥轴退化
//!   的回退链见 [`Directivity`] 文档;
//! - 遮挡:听者→声源**线段**与遮挡线段的最近距 < [`OCCLUSION_EPSILON`]
//!   计一挡,增益用 [`occlusion_gain`] 乘法衰减,由调用方与 [`SourceMix::gain`]
//!   叠乘组合(本层不自动叠);
//! - 确定性:无随机、无隐藏状态、无环境依赖,同输入双跑逐位一致;
//! - 输出不变量:[`SourceMix`] 三字段永远是有限值——任何非有限中间量收敛为
//!   gain=0 / pan=0 / distance=+∞,不 panic、不外泄 NaN。
//!
//! **诚实边界**
//! - 没有 HRTF、没有混响、没有多普勒、没有声速延迟、没有房间几何或多次
//!   反射;遮挡是 0/1 线段计数,没有半影、没有材质吸声;
//! - 声像只有左右一维(等功率/恒功率的左右分配规则留给混音器),俯仰与
//!   前后不进 pan;
//! - 声线是直线,不做绕射:墙后是否"完全静音"由遮挡增益决定而非几何;
//! - `f32` 精度:世界坐标很大(≈1e6 以上)时位置相减丢精度,距离/声像会抖;
//!   大世界请用局部系或分块。

/// 遮挡判挡阈值:遮挡线段与声线的最近距小于此值(世界单位,米)即计一挡。
/// 端点擦碰(最近距 0)算挡。改大可模拟"粗遮挡体",改小则要求几何精确相交。
pub const OCCLUSION_EPSILON: f32 = 1e-3;

/// 长度平方低于此值的向量视作零向量(长度 < 1e-6)。私有:只影响
/// "退化 vs 有效"的判定边界,不影响正常几何。
const ZERO_EPS_SQ: f32 = 1e-12;

/// 单位朝向叉积长度平方(= sin²夹角)低于此值视作平行(夹角 < ~1e-6 rad)。
const PARALLEL_SIN_SQ: f32 = 1e-12;

/// 指向性内外角之差低于此值(度)时线性段退化为阶梯。
const ANGLE_EPS_DEG: f32 = 1e-6;

/// [`Listener::new`] 的朝向校验错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrientationError {
    /// forward 或 up 是零向量(长度 < 1e-6)。
    ZeroVector,
    /// forward 或 up 含 NaN/±∞。
    NonFinite,
    /// forward 与 up 平行或近平行(叉积 sin < 1e-6),右轴不唯一。
    Parallel,
}

impl std::fmt::Display for OrientationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroVector => write!(f, "orientation vector is zero (length < 1e-6)"),
            Self::NonFinite => write!(f, "orientation vector is non-finite (NaN/inf)"),
            Self::Parallel => {
                write!(
                    f,
                    "forward and up are (near-)parallel; right axis undefined"
                )
            }
        }
    }
}

impl std::error::Error for OrientationError {}

/// 听者(头部):位置 + 朝向。
///
/// 用 [`Listener::new`] 构造以获得校验与单位化;字段公开是为了让调用方能
/// 逐帧改 `pos` 而不必重新校验朝向。**字面量构造(绕过 `new`)的退化朝向
/// 不会 panic**:`spatialize` 会把退化右轴的 pan 收敛为 0。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Listener {
    /// 世界坐标(米)。本结构不校验它:非有限 `pos` 由 [`spatialize`] 的
    /// 非有限收敛(增益 0)兜底。
    pub pos: [f32; 3],
    /// "看向"的方向(`new` 校验后单位化存储)。
    pub forward: [f32; 3],
    /// 头顶方向(不必与 forward 正交,只需不平行)。
    pub up: [f32; 3],
}

impl Listener {
    /// 构造并校验听者朝向。
    ///
    /// - `forward`/`up` 必须非零([`OrientationError::ZeroVector`])、有限
    ///   ([`OrientationError::NonFinite`])、且不平行
    ///   ([`OrientationError::Parallel`])——零向量或平行的朝向无法唯一确定
    ///   右轴,构造期报错,不在运行期静默;
    /// - 校验通过后**单位化存储**(后续叉积/点积的数值稳定性);
    /// - `pos` 不在这里校验:非有限 `pos` 由 [`spatialize`] 的非有限收敛
    ///   (增益 0)兜底。
    pub fn new(pos: [f32; 3], forward: [f32; 3], up: [f32; 3]) -> Result<Self, OrientationError> {
        let forward = orient(forward)?;
        let up = orient(up)?;
        let cross = cross3(forward, up);
        if len_sq3(cross) < PARALLEL_SIN_SQ {
            return Err(OrientationError::Parallel);
        }
        Ok(Self { pos, forward, up })
    }
}

/// 声源指向性。
///
/// [`Directivity::Cone`] 的语义(OpenAL 同族):声源有一个发射锥,增益随
/// "声源→听者方向"与锥轴的夹角变化——锥内(夹角 ≤ `inner_deg`,半角、度)
/// 全量 1;锥外(夹角 ≥ `outer_deg`)取 `outer_gain`;中间线性过渡。
///
/// - `axis` 是**世界系**锥轴,约定为"声源朝哪边发":轴指向听者 → 全量;
///   零向量/非有限时**回退到 [`Source::forward`]**,`forward` 也退化则按
///   [`Directivity::Omni`] 处理(不 panic,测试钉死);
/// - `outer_gain` 在使用时 clamp 到 [0, 1](构造不拦、使用兜底——枚举字段
///   公开,任何字面量构造路径都安全);
/// - `inner_deg ≥ outer_deg` 时不报错、退化为阶梯:夹角 ≤ `inner_deg` 取 1,
///   否则取 `outer_gain`(调用方负责传 inner < outer);
/// - 角度或 `outer_gain` 非有限:增益经 [`spatialize`] 收敛为 0(静音)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Directivity {
    /// 全向:不因朝向衰减。
    Omni,
    /// 发射锥:见类型文档。
    Cone {
        /// 锥轴(世界系,使用时单位化;零向量/非有限回退到 `Source::forward`)。
        axis: [f32; 3],
        /// 锥内半角(度),夹角 ≤ 此值全量。
        inner_deg: f32,
        /// 锥外半角(度),夹角 ≥ 此值取 `outer_gain`。
        outer_deg: f32,
        /// 锥外增益,使用时 clamp 到 [0, 1]。
        outer_gain: f32,
    },
}

/// 声源:位置 + 基础增益 + 朝向 + 指向性。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Source {
    /// 世界坐标(米)。
    pub pos: [f32; 3],
    /// 基础线性增益,`new` 构造时 clamp 到 [0, 1]。
    pub gain: f32,
    /// 声源朝向;使用时单位化,零向量/非有限 → 该源按全向处理。
    pub forward: [f32; 3],
    /// 指向性(见 [`Directivity`])。
    pub directivity: Directivity,
}

impl Source {
    /// 构造声源,`gain` clamp 到 [0, 1](非有限收敛为 0)。
    ///
    /// `forward` 与 `directivity` 不在构造时校验——使用时单位化/回退/收敛,
    /// 任何字面量构造路径都不会 panic。
    pub fn new(pos: [f32; 3], gain: f32, forward: [f32; 3], directivity: Directivity) -> Self {
        let gain = if gain.is_finite() {
            gain.clamp(0.0, 1.0)
        } else {
            0.0
        };
        Self {
            pos,
            gain,
            forward,
            directivity,
        }
    }
}

/// 单个声源混入主总线的参数:线性最终增益与声像。
///
/// **输出不变量**:三个字段永远是有限值(见模块文档)。`gain` 已含基础增益
/// × 距离衰减 × 指向性;遮挡衰减不在这里——用 [`occlusion`] + [`occlusion_gain`]
/// 另算,由调用方叠乘。`distance` 供混音器做空气感(低通/混响湿度)之类上层
/// 决策,本层不消费它。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourceMix {
    /// 线性最终增益,clamp 到 [0, 1]。
    pub gain: f32,
    /// 声像:-1 全左、0 居中、+1 全右;左右分配规则(等功率等)留给混音器。
    pub pan: f32,
    /// 听者-声源欧氏距离;非有限输入时为 +∞(不是 0——避免"贴脸却静音"的
    /// 矛盾语义)。
    pub distance: f32,
}

/// 空间化:听者 × 声源 → 混音参数。
///
/// 计算顺序:距离 → 距离门 → 距离衰减 → 声像 → 指向性 → 合成。
///
/// - 距离 `d = |source.pos - listener.pos|`;`d > max_dist` → 增益 0,
///   **但仍返回 pan/distance 正常的完整 [`SourceMix`]——不剔除声源**,是否
///   丢弃由调用方决定;
/// - 衰减 `1 / (1 + falloff_k · d²)`;`falloff_k < 0` 不报错,但会把近距离
///   增益放大(最终仍被 [0,1] clamp 兜底)——语义上请传 ≥ 0;
/// - 声像 = (声源-听者)单位方向 · 右轴,右轴 = normalize(forward × up);
///   听者朝向退化(字面量构造且 forward ∥ up)或非有限 → pan 0;
/// - 重合位置(相距 < 1e-6)→ pan 0、增益满(衰减 1、指向性 1);
/// - 指向性见 [`Directivity`];
/// - 非有限输入(任何位置/增益/角度/参数含 NaN/±∞)→ gain 0、pan 0、
///   distance +∞,不 panic;
/// - `max_dist < 0` 视作 0(只有重合位置可闻);`max_dist = +∞` 合法
///   (永不门限)。
pub fn spatialize(
    listener: &Listener,
    source: &Source,
    max_dist: f32,
    falloff_k: f32,
) -> SourceMix {
    let rel = sub3(source.pos, listener.pos);
    let d = len_sq3(rel).sqrt();
    let dir = norm3(rel); // None = 重合(或退化),pan/锥都按中性处理

    // 距离门:负 max_dist 视作 0,保住"重合 → 增益满"的契约;NaN 距离的
    // 比较恒 false,会落入衰减分支、最后被非有限收敛吃掉。
    let max_dist = max_dist.max(0.0);
    let beyond = d > max_dist;
    let falloff = if beyond {
        0.0
    } else {
        1.0 / (1.0 + falloff_k * d * d)
    };

    // 声像:方向在右轴上的投影。右轴退化(听者字面量构造、forward ∥ up)
    // 或方向退化(重合/非有限)→ 0。
    let right = norm3(cross3(listener.forward, listener.up));
    let pan = match (dir, right) {
        (Some(dir), Some(right)) => dot3(dir, right).clamp(-1.0, 1.0),
        _ => 0.0,
    };

    // 指向性:重合位置声源贴脸,不因朝向衰减。
    let cone = match dir {
        None => 1.0,
        Some(dir) => cone_gain(source, [-dir[0], -dir[1], -dir[2]]),
    };

    // 合成 + 收敛:输出不变量——三字段永远有限。
    let raw = source.gain * falloff * cone;
    let gain = if raw.is_finite() {
        raw.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let distance = if d.is_finite() { d } else { f32::INFINITY };
    SourceMix {
        gain,
        pan,
        distance,
    }
}

/// 锥形指向性增益(0..1)。`to_listener` 是"声源→听者"的单位方向。
///
/// 回退链:锥轴零向量/非有限 → 用 `Source::forward`;也退化 → 全向(1)。
/// 角度非有限时结果为 NaN,由 [`spatialize`] 收敛为 0。
fn cone_gain(source: &Source, to_listener: [f32; 3]) -> f32 {
    let Directivity::Cone {
        axis,
        inner_deg,
        outer_deg,
        outer_gain,
    } = source.directivity
    else {
        return 1.0;
    };
    let Some(axis) = norm3(axis).or_else(|| norm3(source.forward)) else {
        return 1.0;
    };
    let cos = dot3(axis, to_listener).clamp(-1.0, 1.0);
    let angle = cos.acos().to_degrees();
    let outer_gain = if outer_gain.is_finite() {
        outer_gain.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if angle <= inner_deg {
        return 1.0;
    }
    if angle >= outer_deg {
        return outer_gain;
    }
    let denom = outer_deg - inner_deg;
    if denom <= ANGLE_EPS_DEG {
        // 内外角重叠(inner ≥ outer):阶梯边界在 inner。
        return outer_gain;
    }
    let t = (angle - inner_deg) / denom;
    1.0 + t * (outer_gain - 1.0)
}

/// 数一数听者→声源的声线被多少条线段挡住。
///
/// - 声线取**听者到声源的线段**(不是无限射线):听者背后、声源以外的遮挡物
///   不计入;
/// - 判挡:遮挡线段与声线的最近距离 < [`OCCLUSION_EPSILON`](端点擦碰、恰好
///   穿过都算);
/// - 听者/声源重合时声线退化为点,按"点-线段"距离判;
/// - 非有限输入(听者/声源位置或遮挡端点含 NaN/±∞):该段**不计入**——宁可
///   漏挡、不 panic(诚实定义,模块文档有总述)。
pub fn occlusion(listener_pos: [f32; 3], source_pos: [f32; 3], blockers: &[[[f32; 3]; 2]]) -> u32 {
    let mut hits = 0u32;
    for seg in blockers {
        let dist = seg_seg_distance(listener_pos, source_pos, seg[0], seg[1]);
        if dist.is_finite() && dist < OCCLUSION_EPSILON {
            hits += 1;
        }
    }
    hits
}

/// 遮挡增益:乘法衰减 `per_block^hits`,clamp 下限 0。
///
/// - `hits == 0` → 恒 1(没有遮挡就没有衰减,连 `per_block` 是 0/NaN 也一样);
/// - `per_block` 先 clamp 到 [0, 1](>1 视作 1、负值视作 0、非有限视作 0);
/// - `hits` 很大时结果下溢到 0——乘法衰减的数学极限,不是错误。
pub fn occlusion_gain(hits: u32, per_block: f32) -> f32 {
    if hits == 0 {
        return 1.0;
    }
    let p = if per_block.is_finite() {
        per_block.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let exp = hits.min(i32::MAX as u32) as i32;
    p.powi(exp).max(0.0)
}

/// 两根 3D 线段的最近距离(Ericson, Real-Time Collision Detection §5.1.9)。
/// 退化线段(两端重合)按点-线段处理;NaN/±∞ 输入传播为 NaN,由 [`occlusion`]
/// 判弃(不计挡)。
fn seg_seg_distance(p1: [f32; 3], q1: [f32; 3], p2: [f32; 3], q2: [f32; 3]) -> f32 {
    const DEGEN: f32 = ZERO_EPS_SQ;
    let ray_dir = sub3(q1, p1);
    let blk_dir = sub3(q2, p2);
    let offset = sub3(p1, p2);
    let ray_len_sq = len_sq3(ray_dir);
    let blk_len_sq = len_sq3(blk_dir);
    let (ray_t, blk_t) = if ray_len_sq <= DEGEN && blk_len_sq <= DEGEN {
        (0.0, 0.0) // 两根都退化为点:距离即 |p1-p2|
    } else if ray_len_sq <= DEGEN {
        // 声线退化为点:投到遮挡段上
        (0.0, (dot3(blk_dir, offset) / blk_len_sq).clamp(0.0, 1.0))
    } else if blk_len_sq <= DEGEN {
        // 遮挡段退化为点
        ((-dot3(ray_dir, offset) / ray_len_sq).clamp(0.0, 1.0), 0.0)
    } else {
        let dirs_dot = dot3(ray_dir, blk_dir);
        let proj_a = dot3(ray_dir, offset);
        let proj_b = dot3(blk_dir, offset);
        let denom = ray_len_sq * blk_len_sq - dirs_dot * dirs_dot;
        let mut ray_t = if denom > DEGEN {
            ((dirs_dot * proj_b - proj_a * blk_len_sq) / denom).clamp(0.0, 1.0)
        } else {
            0.0 // 近平行:取声线起点试投影
        };
        let mut blk_t = (dirs_dot * ray_t + proj_b) / blk_len_sq;
        if blk_t < 0.0 {
            blk_t = 0.0;
            ray_t = (-proj_a / ray_len_sq).clamp(0.0, 1.0);
        } else if blk_t > 1.0 {
            blk_t = 1.0;
            ray_t = ((dirs_dot - proj_a) / ray_len_sq).clamp(0.0, 1.0);
        }
        (ray_t, blk_t)
    };
    let near_ray = add3(p1, mul3(ray_dir, ray_t));
    let near_blk = add3(p2, mul3(blk_dir, blk_t));
    len_sq3(sub3(near_ray, near_blk)).sqrt()
}

// --- 私有小向量工具(纯 std,无依赖) ---

fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn mul3(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn len_sq3(a: [f32; 3]) -> f32 {
    dot3(a, a)
}

fn is_finite3(a: [f32; 3]) -> bool {
    a[0].is_finite() && a[1].is_finite() && a[2].is_finite()
}

/// 单位化;零向量/非有限 → None(调用方按退化处理,不 panic)。
fn norm3(a: [f32; 3]) -> Option<[f32; 3]> {
    let sq = len_sq3(a);
    if !sq.is_finite() || sq < ZERO_EPS_SQ {
        return None;
    }
    let n = sq.sqrt();
    Some([a[0] / n, a[1] / n, a[2] / n])
}

/// 朝向向量校验 + 单位化([`Listener::new`] 用)。
fn orient(v: [f32; 3]) -> Result<[f32; 3], OrientationError> {
    if !is_finite3(v) {
        return Err(OrientationError::NonFinite);
    }
    norm3(v).ok_or(OrientationError::ZeroVector)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认听者:原点,看向 -z,头顶 +y(右轴 = +x)。
    fn listener() -> Listener {
        Listener::new([0.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]).expect("合法朝向")
    }

    /// 全向、满增益声源。
    fn src(pos: [f32; 3]) -> Source {
        Source::new(pos, 1.0, [0.0, 0.0, -1.0], Directivity::Omni)
    }

    /// 混音三字段的位模式(逐位比较用,NaN 也能比)。
    fn mix_bits(m: &SourceMix) -> [u32; 3] {
        [m.gain.to_bits(), m.pan.to_bits(), m.distance.to_bits()]
    }

    // --- 声像 ---

    #[test]
    fn pan_front_zero_right_one_left_minus_one() {
        let l = listener();
        // 正前(forward 方向)= 0;正右 = +1;正左 = -1;正后、头顶 = 0
        // (pan 只管左右,俯仰/前后不进)。
        assert_eq!(spatialize(&l, &src([0.0, 0.0, -5.0]), 100.0, 0.0).pan, 0.0);
        assert_eq!(spatialize(&l, &src([5.0, 0.0, 0.0]), 100.0, 0.0).pan, 1.0);
        assert_eq!(spatialize(&l, &src([-5.0, 0.0, 0.0]), 100.0, 0.0).pan, -1.0);
        assert_eq!(spatialize(&l, &src([0.0, 0.0, 5.0]), 100.0, 0.0).pan, 0.0);
        assert_eq!(spatialize(&l, &src([0.0, 5.0, 0.0]), 100.0, 0.0).pan, 0.0);
        // 45° 方向 = 1/√2(右前)与 -1/√2(左前)。
        let right_front = spatialize(&l, &src([5.0, 0.0, -5.0]), 100.0, 0.0).pan;
        let left_front = spatialize(&l, &src([-5.0, 0.0, -5.0]), 100.0, 0.0).pan;
        assert!((right_front - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert!((left_front + std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
    }

    // --- 距离衰减与距离门 ---

    #[test]
    fn attenuation_monotonic_and_exact_formula() {
        let l = listener();
        let m1 = spatialize(&l, &src([0.0, 0.0, -1.0]), 1e6, 1.0);
        assert_eq!(m1.gain, 0.5, "k=1、d=1 → 1/(1+1)");
        assert_eq!(m1.distance, 1.0);
        let m2 = spatialize(&l, &src([0.0, 0.0, -2.0]), 1e6, 1.0);
        assert_eq!(m2.gain, 0.2, "k=1、d=2 → 1/5");
        let m3 = spatialize(&l, &src([0.0, 0.0, -3.0]), 1e6, 1.0);
        assert_eq!(m3.gain, 0.1, "k=1、d=3 → 1/10");
        assert!(m1.gain > m2.gain && m2.gain > m3.gain, "随距离单调降");
        let m4 = spatialize(&l, &src([0.0, 0.0, -3.0]), 1e6, 2.0);
        assert_eq!(m4.gain, 1.0 / 19.0, "k=2、d=3 → 1/(1+18),与手算逐位一致");
        // k=0:不衰减。
        let m0 = spatialize(&l, &src([0.0, 0.0, -50.0]), 1e6, 0.0);
        assert_eq!(m0.gain, 1.0);
    }

    #[test]
    fn beyond_max_dist_gain_zero_but_mix_still_full() {
        let l = listener();
        // d == max_dist:门是严格大于,边界上仍在门内。
        let on_edge = spatialize(&l, &src([6.0, 0.0, 0.0]), 6.0, 1.0);
        assert_eq!(on_edge.distance, 6.0);
        assert!(on_edge.gain > 0.0, "d == max_dist 不剪");
        // d > max_dist:增益 0,但 Mix 完整——pan/distance 仍正常,不剔除。
        let off = spatialize(&l, &src([6.0, 0.0, 0.0]), 5.0, 1.0);
        assert_eq!(off.gain, 0.0);
        assert_eq!(off.pan, 1.0, "门外声像仍在");
        assert_eq!(off.distance, 6.0);
    }

    // --- 指向性 ---

    #[test]
    fn cone_inner_one_outer_gain_linear_between() {
        let l = listener();
        let cone = Directivity::Cone {
            axis: [0.0, 0.0, -1.0],
            inner_deg: 10.0,
            outer_deg: 30.0,
            outer_gain: 0.25,
        };
        let source = Source::new([0.0; 3], 1.0, [0.0, 0.0, -1.0], cone);
        // 听者放在距源 10 米、与锥轴成 θ 角的方向上;k=0 隔离衰减,只看锥。
        let place = |deg: f32| {
            let a = deg.to_radians();
            [10.0 * a.sin(), 0.0, -10.0 * a.cos()]
        };
        let gain_at = |deg: f32| -> f32 {
            let listener = Listener::new(place(deg), l.forward, l.up).expect("合法朝向");
            spatialize(&listener, &source, 1e4, 0.0).gain
        };
        // 轴正对:全量。
        assert_eq!(spatialize(&l, &source, 1e4, 0.0).gain, 1.0);
        // 锥内(θ=5° < inner):精确 1。
        assert_eq!(gain_at(5.0), 1.0);
        // inner 边界(θ=10°):容差断言(acos 的 f32 舍入可能跨过边界)。
        assert!((gain_at(10.0) - 1.0).abs() < 1e-5);
        // 中间(θ=20°):线性 t=0.5 → 1 - 0.5·(1-0.25) = 0.625。
        assert!((gain_at(20.0) - 0.625).abs() < 1e-4);
        // outer 边界(θ=30°):容差断言。
        assert!((gain_at(30.0) - 0.25).abs() < 1e-5);
        // 锥外(θ=35°):精确 outer_gain。
        assert_eq!(gain_at(35.0), 0.25);
    }

    #[test]
    fn cone_degenerate_axis_falls_back_then_omni() {
        let l_front =
            Listener::new([0.0, 0.0, -10.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]).expect("合法朝向");
        let cone0 = Directivity::Cone {
            axis: [0.0; 3],
            inner_deg: 10.0,
            outer_deg: 30.0,
            outer_gain: 0.25,
        };
        // 轴零向量 → 回退到 Source::forward;听者在 forward 方向上 → 全量。
        let s_fallback = Source::new([0.0; 3], 1.0, [0.0, 0.0, -1.0], cone0);
        assert_eq!(spatialize(&l_front, &s_fallback, 1e4, 0.0).gain, 1.0);
        // forward 背对听者 → 锥外。
        let s_back = Source::new([0.0; 3], 1.0, [0.0, 0.0, 1.0], cone0);
        assert_eq!(spatialize(&l_front, &s_back, 1e4, 0.0).gain, 0.25);
        // forward 也退化 → 按 Omni 处理,与 Omni 声源逐位一致。
        let s_zombie = Source {
            pos: [0.0; 3],
            gain: 1.0,
            forward: [0.0; 3],
            directivity: cone0,
        };
        let omni = Source::new([0.0; 3], 1.0, [0.0; 3], Directivity::Omni);
        assert_eq!(
            spatialize(&l_front, &s_zombie, 1e4, 0.0).gain.to_bits(),
            spatialize(&l_front, &omni, 1e4, 0.0).gain.to_bits()
        );
    }

    #[test]
    fn cone_degenerate_angles_step_and_outer_gain_clamped() {
        let l = listener();
        // inner ≥ outer:退化为阶梯,边界在 inner(≤60° 全量,>60° outer_gain)。
        let step = Directivity::Cone {
            axis: [0.0, 0.0, -1.0],
            inner_deg: 60.0,
            outer_deg: 30.0,
            outer_gain: 0.1,
        };
        let s_step = Source::new([0.0; 3], 1.0, [0.0, 0.0, -1.0], step);
        let place = |deg: f32| {
            let a = deg.to_radians();
            [10.0 * a.sin(), 0.0, -10.0 * a.cos()]
        };
        let in_step = Listener::new(place(20.0), l.forward, l.up).expect("合法朝向");
        assert_eq!(spatialize(&in_step, &s_step, 1e4, 0.0).gain, 1.0);
        let out_step = Listener::new(place(70.0), l.forward, l.up).expect("合法朝向");
        assert_eq!(spatialize(&out_step, &s_step, 1e4, 0.0).gain, 0.1);
        // outer_gain > 1 使用时 clamp 到 1。
        let hot = Directivity::Cone {
            axis: [0.0, 0.0, -1.0],
            inner_deg: 10.0,
            outer_deg: 30.0,
            outer_gain: 5.0,
        };
        let s_hot = Source::new([0.0; 3], 1.0, [0.0, 0.0, -1.0], hot);
        let outside = Listener::new(place(35.0), l.forward, l.up).expect("合法朝向");
        assert_eq!(
            spatialize(&outside, &s_hot, 1e4, 0.0).gain,
            1.0,
            "5.0 → clamp 1"
        );
    }

    // --- 重合位置 ---

    #[test]
    fn coincident_positions_full_gain_zero_pan() {
        let l =
            Listener::new([2.0, 3.0, 4.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]).expect("合法朝向");
        // 锥 outer_gain=0:任何非重合方向都会静音,重合却满——钉死"重合不吃锥"。
        let s = Source::new(
            [2.0, 3.0, 4.0],
            0.7,
            [1.0, 0.0, 0.0],
            Directivity::Cone {
                axis: [0.0, 0.0, -1.0],
                inner_deg: 10.0,
                outer_deg: 30.0,
                outer_gain: 0.0,
            },
        );
        let m = spatialize(&l, &s, 10.0, 3.0);
        assert_eq!(m.pan, 0.0);
        assert_eq!(m.gain, 0.7, "重合 → 衰减 1、锥 1,增益满(=基础增益)");
        assert_eq!(m.distance, 0.0);
        // max_dist = 0:门内只剩重合位置,仍可闻。
        let m0 = spatialize(&l, &s, 0.0, 1.0);
        assert_eq!(m0.gain, 0.7);
    }

    // --- 非有限输入 ---

    #[test]
    fn non_finite_inputs_converge_silently() {
        let l = listener();
        // 声源位置 NaN:gain 0、pan 0、distance +∞。
        let m = spatialize(&l, &src([f32::NAN, 0.0, 0.0]), 100.0, 1.0);
        assert_eq!(m.gain.to_bits(), 0);
        assert_eq!(m.pan.to_bits(), 0);
        assert!(m.distance.is_infinite() && m.distance > 0.0);
        // 声源位置 +∞:同样收敛。
        let m = spatialize(&l, &src([f32::INFINITY, 0.0, 0.0]), 100.0, 1.0);
        assert_eq!(m.gain.to_bits(), 0);
        assert!(m.distance.is_infinite());
        // 听者位置 NaN(字面量构造):gain 0、distance +∞。
        let l_nan = Listener {
            pos: [f32::NAN; 3],
            forward: [0.0, 0.0, -1.0],
            up: [0.0, 1.0, 0.0],
        };
        let m = spatialize(&l_nan, &src([0.0, 0.0, -1.0]), 100.0, 1.0);
        assert_eq!(m.gain.to_bits(), 0);
        assert!(m.distance.is_infinite());
        // 字面量构造的 NaN gain(new 会拦,这里走 spatialize 兜底)。
        let s_nan = Source {
            pos: [0.0, 0.0, -1.0],
            gain: f32::NAN,
            forward: [0.0; 3],
            directivity: Directivity::Omni,
        };
        let m = spatialize(&l, &s_nan, 100.0, 1.0);
        assert_eq!(m.gain.to_bits(), 0);
        assert_eq!(m.distance.to_bits(), 1.0f32.to_bits());
        // falloff_k NaN:gain 0,距离字段不受影响。
        let m = spatialize(&l, &src([0.0, 0.0, -1.0]), 100.0, f32::NAN);
        assert_eq!(m.gain.to_bits(), 0);
        assert_eq!(m.distance, 1.0);
        // 听者朝向 NaN(字面量构造):只废 pan,不废增益。
        let l_bad = Listener {
            pos: [0.0; 3],
            forward: [f32::NAN, 0.0, 0.0],
            up: [0.0, 1.0, 0.0],
        };
        let m = spatialize(&l_bad, &src([0.0, 0.0, -1.0]), 100.0, 0.0);
        assert_eq!(m.pan.to_bits(), 0);
        assert_eq!(m.gain, 1.0);
        // max_dist = +∞ 合法:永不门限。
        let m = spatialize(&l, &src([0.0, 0.0, -1.0]), f32::INFINITY, 1.0);
        assert_eq!(m.gain, 0.5);
    }

    // --- 构造校验 ---

    #[test]
    fn listener_new_validates_orientation() {
        // 合法:OK 且单位化存储(非单位输入归一)。
        let l = Listener::new([0.0; 3], [2.0, 0.0, -2.0], [0.0, 5.0, 0.0]).expect("合法朝向");
        let n = len_sq3(l.forward).sqrt();
        assert!((n - 1.0).abs() < 1e-6, "forward 应单位化: {n}");
        assert!((l.forward[0] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert_eq!(l.up, [0.0, 1.0, 0.0]);
        // 零向量。
        assert_eq!(
            Listener::new([0.0; 3], [0.0; 3], [0.0, 1.0, 0.0]),
            Err(OrientationError::ZeroVector)
        );
        assert_eq!(
            Listener::new([0.0; 3], [0.0, 0.0, -1.0], [0.0; 3]),
            Err(OrientationError::ZeroVector)
        );
        // 平行(同向、反向、近平行)。
        assert_eq!(
            Listener::new([0.0; 3], [0.0, 0.0, -1.0], [0.0, 0.0, -1.0]),
            Err(OrientationError::Parallel)
        );
        assert_eq!(
            Listener::new([0.0; 3], [0.0, 0.0, -1.0], [0.0, 0.0, 1.0]),
            Err(OrientationError::Parallel)
        );
        assert_eq!(
            Listener::new([0.0; 3], [1.0, 0.0, 0.0], [1.0, 1e-7, 0.0]),
            Err(OrientationError::Parallel),
            "倾角 ~1e-7 rad 视作平行"
        );
        // 非有限。
        assert_eq!(
            Listener::new([0.0; 3], [f32::NAN, 0.0, 0.0], [0.0, 1.0, 0.0]),
            Err(OrientationError::NonFinite)
        );
        assert_eq!(
            Listener::new([0.0; 3], [0.0, 0.0, -1.0], [f32::INFINITY; 3]),
            Err(OrientationError::NonFinite)
        );
        // 小但真实的倾角(1e-3 rad)合法。
        assert!(Listener::new([0.0; 3], [1.0, 0.0, 0.0], [1.0, 1e-3, 0.0]).is_ok());
    }

    #[test]
    fn source_new_clamps_gain() {
        assert_eq!(
            Source::new([0.0; 3], 2.0, [0.0; 3], Directivity::Omni).gain,
            1.0
        );
        assert_eq!(
            Source::new([0.0; 3], -0.5, [0.0; 3], Directivity::Omni).gain,
            0.0
        );
        assert_eq!(
            Source::new([0.0; 3], f32::NAN, [0.0; 3], Directivity::Omni).gain,
            0.0,
            "非有限 gain 构造即收敛为 0"
        );
    }

    // --- 遮挡 ---

    #[test]
    fn occlusion_counts_blocked_rays_and_multiplies() {
        let lp = [0.0, 0.0, 0.0];
        let sp = [0.0, 0.0, 10.0];
        let wall1 = [[-1.0, 0.0, 5.0], [1.0, 0.0, 5.0]];
        let wall2 = [[-1.0, 0.0, 7.0], [1.0, 0.0, 7.0]];
        assert_eq!(occlusion(lp, sp, &[]), 0);
        assert_eq!(occlusion(lp, sp, &[wall1]), 1);
        assert_eq!(occlusion(lp, sp, &[wall1, wall2]), 2);
        // 乘法增益:0.5² = 0.25;随挡数单调降;无遮挡恒 1(per_block 是 0 也一样)。
        assert_eq!(occlusion_gain(2, 0.5), 0.25);
        assert_eq!(occlusion_gain(1, 0.5), 0.5);
        assert!(occlusion_gain(1, 0.5) > occlusion_gain(2, 0.5));
        assert_eq!(occlusion_gain(0, 0.5), 1.0);
        assert_eq!(occlusion_gain(0, 0.0), 1.0);
        assert_eq!(occlusion_gain(0, f32::NAN), 1.0);
    }

    #[test]
    fn occlusion_ignores_off_axis_and_out_of_span() {
        let lp = [0.0, 0.0, 0.0];
        let sp = [0.0, 0.0, 10.0];
        // 侧面 1 米开外:最近距 1 > 阈值,不挡。
        let side = [[1.0, 0.0, 5.0], [2.0, 0.0, 5.0]];
        assert_eq!(occlusion(lp, sp, &[side]), 0);
        // 声线是线段不是无限射线:听者身后、声源以外都不挡。
        let behind = [[-1.0, 0.0, -5.0], [1.0, 0.0, -5.0]];
        let beyond = [[-1.0, 0.0, 15.0], [1.0, 0.0, 15.0]];
        assert_eq!(occlusion(lp, sp, &[behind, beyond]), 0);
        // 端点擦碰(最近距 0)算挡。
        let graze = [[0.0, 0.0, 5.0], [5.0, 0.0, 5.0]];
        assert_eq!(occlusion(lp, sp, &[graze]), 1);
        // 斜穿也算。
        let diagonal = [[-1.0, 0.0, 6.0], [1.0, 0.0, 4.0]];
        assert_eq!(occlusion(lp, sp, &[diagonal]), 1);
        // 退化遮挡段(点)恰好落在声线上:算挡。
        let point_on_ray = [[0.0, 0.0, 5.0], [0.0, 0.0, 5.0]];
        assert_eq!(occlusion(lp, sp, &[point_on_ray]), 1);
        // 声线退化(听者=声源):按点-线段距离判。
        let touching = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]];
        assert_eq!(occlusion(lp, lp, &[touching]), 1);
    }

    #[test]
    fn occlusion_non_finite_never_counts() {
        let lp = [0.0, 0.0, 0.0];
        let sp = [0.0, 0.0, 10.0];
        let nan_seg = [[f32::NAN, 0.0, 5.0], [1.0, 0.0, 5.0]];
        let inf_seg = [[f32::INFINITY, 0.0, 5.0], [1.0, 0.0, 5.0]];
        assert_eq!(
            occlusion(lp, sp, &[nan_seg, inf_seg]),
            0,
            "宁可漏挡,不 panic"
        );
        // 听者/声源位置非有限:同样不计。
        assert_eq!(occlusion([f32::NAN; 3], sp, &[nan_seg]), 0);
        // 增益侧:per_block clamp(>1→1、负→0、非有限→0);hits=0 恒 1;大 hits 下溢到 0。
        assert_eq!(occlusion_gain(1, 2.0), 1.0);
        assert_eq!(occlusion_gain(1, -0.5), 0.0);
        assert_eq!(occlusion_gain(1, f32::NAN), 0.0);
        assert_eq!(occlusion_gain(1, f32::INFINITY), 0.0, "非有限视作 0(全挡)");
        assert_eq!(occlusion_gain(1_000, 0.5), 0.0, "0.5^1000 下溢到 0");
    }

    // --- 确定性 ---

    #[test]
    fn same_inputs_are_bit_identical() {
        // 两个独立构造但参数相同的听者:存储逐位一致。
        let l1 =
            Listener::new([0.5, 1.0, 2.0], [0.1, 0.0, -1.0], [0.0, 1.0, 0.1]).expect("合法朝向");
        let l2 =
            Listener::new([0.5, 1.0, 2.0], [0.1, 0.0, -1.0], [0.0, 1.0, 0.1]).expect("合法朝向");
        assert_eq!(l1, l2);
        // 一组覆盖边界情形的脚本:重合、门外、NaN 位置、NaN 参数。
        let script: [([f32; 3], f32, f32); 6] = [
            ([0.0, 0.0, -5.0], 50.0, 1.0),
            ([5.0, 0.0, 0.0], 50.0, 1.0),
            ([0.5, 1.0, 2.0], 10.0, 3.0),   // 与听者重合
            ([100.0, 0.0, 0.0], 50.0, 1.0), // 门外
            ([f32::NAN, 0.0, 0.0], 50.0, 1.0),
            ([0.0, 0.0, -1.0], 50.0, f32::NAN),
        ];
        for (pos, max_dist, k) in script {
            let s = Source::new(
                pos,
                0.8,
                [0.2, 0.0, -1.0],
                Directivity::Cone {
                    axis: [0.0, 0.0, -1.0],
                    inner_deg: 15.0,
                    outer_deg: 45.0,
                    outer_gain: 0.3,
                },
            );
            let a = spatialize(&l1, &s, max_dist, k);
            let b = spatialize(&l2, &s, max_dist, k);
            assert_eq!(mix_bits(&a), mix_bits(&b), "同输入必须逐位一致: {pos:?}");
        }
        // 遮挡计数与增益同样双跑逐位(含 NaN 段)。
        let blockers = [
            [[-1.0, 0.0, 5.0], [1.0, 0.0, 5.0]],
            [[0.0, 0.0, 5.0], [5.0, 0.0, 5.0]],
            [[f32::NAN, 0.0, 5.0], [1.0, 0.0, 5.0]],
        ];
        let h1 = occlusion([0.0; 3], [0.0, 0.0, 10.0], &blockers);
        let h2 = occlusion([0.0; 3], [0.0, 0.0, 10.0], &blockers);
        assert_eq!(h1, h2);
        assert_eq!(
            occlusion_gain(h1, 0.7).to_bits(),
            occlusion_gain(h2, 0.7).to_bits()
        );
    }
}
