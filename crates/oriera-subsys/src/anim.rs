//! L6 动画状态机（feature `anim`）— 剪辑采样 + 三态迟滞 + 交叉淡入。
//!
//! 对应 QY-Demo 拆解第二档的「动画状态机」：蒙皮渲染（RFC 0004 步 1c）已经有
//! 了，缺的是"谁来驱动关节面板"。本模块把"速度 → 状态 → 面板"这条链补上，
//! 输出正是 `update_skeleton` 要的 `[[f32;4];4]` 列表，纯 CPU、可确定性测试。
//!
//! **契约**
//! - [`AnimationClip`] 是"每关节若干关键帧"的 TRS 轨道，时间以秒计、循环播放；
//!   采样按 `t.rem_euclid(duration)` 取相位，所以负时间与超长时间都能折回。
//! - [`AnimController`] 只看**速度标量**决定状态（Idle / Walk / Run），阈值两侧
//!   各留一条迟滞带；速度在带内抖动不会来回切。切状态时把旧状态的相位留一份，
//!   按 `fade` 时长交叉淡入。
//! - [`AnimController::sample`] 每帧推进一次时钟并返回面板，长度 = 三个剪辑里
//!   最大的关节数，缺轨道的关节输出单位矩阵（bind 姿势）。
//! - **确定性**：无哈希、无随机、无浮点累加之外的隐藏状态；同一串输入两次跑
//!   面板逐位一致。相位用 `f32` 累加，长跑会积累误差——按帧数每 10 分钟误差在
//!   毫秒量级，回放对齐请以帧号为准，不要以相位为准。
//!
//! **诚实边界**
//! - 混合是**矩阵按元素线性插值**，不是 TRS 空间混合：大角度旋转会缩水
//!   （这是几乎所有简易实现都有的问题）。要正确就得在四元数/双四元数上做，
//!   本步不做，`blend_matrices` 的文档里写明。
//! - 只驱动关节，不做 IK、不做根运动（root motion）、不做脚部贴地。
//! - 状态是"速度驱动"的三态，没有事件轨道、没有上层强制打断（需要就在上层
//!   直接改 `speed` 或换 `AnimController::force`）。
//! - 剪辑必须自带 bind 之外的**完整**关节姿态；缺的关节按单位矩阵补，不做
//!   父级继承推导。

/// 一个关节关键帧：位置 + 旋转（四元数 x,y,z,w）+ 缩放。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Keyframe {
    pub t: f32,
    pub pos: [f32; 3],
    pub rot: [f32; 4],
    pub scale: [f32; 3],
}

impl Keyframe {
    pub fn new(t: f32, pos: [f32; 3], rot: [f32; 4], scale: [f32; 3]) -> Self {
        Self { t, pos, rot, scale }
    }

    /// 只有平移的关键帧（最常用的简写）。
    pub fn at(t: f32, pos: [f32; 3]) -> Self {
        Self {
            t,
            pos,
            rot: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0],
        }
    }
}

/// 一个关节的轨道。`joint` 是面板里的下标（0 起）。
#[derive(Debug, Clone, PartialEq)]
pub struct JointTrack {
    pub joint: usize,
    pub keys: Vec<Keyframe>,
}

/// 一段循环剪辑。
#[derive(Debug, Clone, PartialEq)]
pub struct AnimationClip {
    pub name: String,
    /// 循环长度（秒）。≤ 0 的剪辑被视作静态，采样总取第一帧。
    pub duration: f32,
    pub tracks: Vec<JointTrack>,
}

impl AnimationClip {
    pub fn new(name: impl Into<String>, duration: f32, tracks: Vec<JointTrack>) -> Self {
        Self {
            name: name.into(),
            duration,
            tracks,
        }
    }

    /// 这段剪辑覆盖到的最大关节号 + 1（没有轨道时为 0）。
    pub fn joint_count(&self) -> usize {
        self.tracks.iter().map(|t| t.joint + 1).max().unwrap_or(0)
    }

    /// 把 `time` 时刻的姿态写进 `palette`（长度不足就长到刚够，已有的关节不覆盖
    /// 未出现在本剪辑里的那些——那些由调用方或 [`blend_matrices`] 负责）。
    pub fn sample_into(&self, time: f32, palette: &mut Vec<[[f32; 4]; 4]>) {
        let need = self.joint_count();
        if palette.len() < need {
            palette.resize(need, IDENTITY);
        }
        let t = if self.duration > 0.0 {
            time.rem_euclid(self.duration)
        } else {
            0.0
        };
        for track in &self.tracks {
            if let Some(m) = sample_track(track, t, self.duration) {
                palette[track.joint] = m;
            }
        }
    }
}

/// 单位矩阵。
pub const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// TRS → 行主序 4×4（平移在 `m[0..3][3]`，底行 `[0,0,0,1]`）。与渲染侧
/// `update_skeleton` / 实例矩阵同一约定（它们上传时再转列序）。
pub fn trs_matrix(k: &Keyframe) -> [[f32; 4]; 4] {
    let q = normalize_quat(k.rot);
    let (x, y, z, w) = (q[0], q[1], q[2], q[3]);
    let r = [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y - w * z),
            2.0 * (x * z + w * y),
        ],
        [
            2.0 * (x * y + w * z),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z - w * x),
        ],
        [
            2.0 * (x * z - w * y),
            2.0 * (y * z + w * x),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ];
    let mut m = IDENTITY;
    for row in 0..3 {
        for col in 0..3 {
            m[row][col] = r[row][col] * k.scale[col];
        }
        m[row][3] = k.pos[row];
    }
    m
}

fn normalize_quat(q: [f32; 4]) -> [f32; 4] {
    let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if n <= 1e-6 || !n.is_finite() {
        return [0.0, 0.0, 0.0, 1.0];
    }
    [q[0] / n, q[1] / n, q[2] / n, q[3] / n]
}

/// 两个关键帧之间插值：位置/缩放线性，旋转走"最短路径的线性插值 + 归一化"
/// （nlerp）。nlerp 不是等角速度，够用且不会像 lerp 那样缩水。
fn lerp_key(a: &Keyframe, b: &Keyframe, alpha: f32) -> Keyframe {
    let alpha = alpha.clamp(0.0, 1.0);
    let mut q0 = a.rot;
    let mut q1 = b.rot;
    let dot = q0[0] * q1[0] + q0[1] * q1[1] + q0[2] * q1[2] + q0[3] * q1[3];
    if dot < 0.0 {
        q1 = [-q1[0], -q1[1], -q1[2], -q1[3]];
    }
    if dot.abs() < 1e-6 {
        q0 = [0.0, 0.0, 0.0, 1.0];
    }
    let rot = normalize_quat([
        q0[0] + (q1[0] - q0[0]) * alpha,
        q0[1] + (q1[1] - q0[1]) * alpha,
        q0[2] + (q1[2] - q0[2]) * alpha,
        q0[3] + (q1[3] - q0[3]) * alpha,
    ]);
    let mut out = Keyframe {
        t: a.t + (b.t - a.t) * alpha,
        pos: [0.0; 3],
        rot,
        scale: [0.0; 3],
    };
    for i in 0..3 {
        out.pos[i] = a.pos[i] + (b.pos[i] - a.pos[i]) * alpha;
        out.scale[i] = a.scale[i] + (b.scale[i] - a.scale[i]) * alpha;
    }
    out
}

/// 单条轨道在时刻 `t` 的姿态。`duration` 用来跨"末帧 → 首帧"循环。
///
/// 关键帧假定按时间升序；跨段插值的 `alpha` 会被夹到 `[0,1]`，所以时间戳
/// 乱序不会炸，只会退化（该关节停在最近的关键帧上）。
fn sample_track(track: &JointTrack, t: f32, duration: f32) -> Option<[[f32; 4]; 4]> {
    let keys = &track.keys;
    if keys.is_empty() {
        return None;
    }
    if keys.len() == 1 {
        return Some(trs_matrix(&keys[0]));
    }
    let mut prev_i: Option<usize> = None;
    for (i, k) in keys.iter().enumerate() {
        if k.t <= t {
            prev_i = Some(i);
        }
    }
    let (pi, ni) = match prev_i {
        Some(i) if i + 1 < keys.len() => (i, i + 1),
        Some(i) => (i, 0),           // 越过最后一帧 → 回到首帧（循环）
        None => (keys.len() - 1, 0), // 早于首帧 → 从末帧绕过来
    };
    let a = &keys[pi];
    let b = &keys[ni];
    let tb = if ni == 0 && b.t <= a.t {
        b.t + duration.max(a.t + 1e-3)
    } else {
        b.t
    };
    let ta = if prev_i.is_none() {
        a.t - duration.max(a.t + 1e-3)
    } else {
        a.t
    };
    let span = tb - ta;
    let alpha = if span > 1e-6 { (t - ta) / span } else { 0.0 };
    Some(trs_matrix(&lerp_key(a, b, alpha)))
}

/// 两个面板按元素线性插值：`out = a*(1-w) + b*w`。
///
/// **这是近似**：矩阵线性插值不等于姿态插值，大角度旋转会缩水（长度小于 1）。
/// 简易实现普遍如此；要正确得在四元数或双四元数空间混。
pub fn blend_matrices(a: &[[f32; 4]; 4], b: &[[f32; 4]; 4], w: f32) -> [[f32; 4]; 4] {
    let w = w.clamp(0.0, 1.0);
    let mut m = IDENTITY;
    for r in 0..4 {
        for c in 0..4 {
            m[r][c] = a[r][c] * (1.0 - w) + b[r][c] * w;
        }
    }
    m
}

/// 三态行走状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locomotion {
    Idle,
    Walk,
    Run,
}

/// 三态各一段剪辑。
#[derive(Debug, Clone, PartialEq)]
pub struct ClipBank {
    pub idle: AnimationClip,
    pub walk: AnimationClip,
    pub run: AnimationClip,
}

impl ClipBank {
    pub fn clip(&self, state: Locomotion) -> &AnimationClip {
        match state {
            Locomotion::Idle => &self.idle,
            Locomotion::Walk => &self.walk,
            Locomotion::Run => &self.run,
        }
    }

    /// 三个剪辑里最大的关节数——输出面板的长度。
    pub fn joint_count(&self) -> usize {
        self.idle
            .joint_count()
            .max(self.walk.joint_count())
            .max(self.run.joint_count())
    }
}

/// 速度驱动的三态状态机 + 交叉淡入。
///
/// 阈值语义：`speed > walk_at + h/2` 才进 Walk，`speed < walk_at - h/2` 才回
/// Idle（Run 同理）。带内维持现状，所以速度在阈值附近抖动不会来回切。
///
/// 切状态时相位归零、旧状态相位留一份，按 `fade_time` 淡入；`fade_time = 0`
/// 就是硬切。
#[derive(Debug, Clone, PartialEq)]
pub struct AnimController {
    state: Locomotion,
    clock: f32,
    prev: Option<(Locomotion, f32)>,
    fade: f32,
    pub walk_at: f32,
    pub run_at: f32,
    pub hysteresis: f32,
    pub fade_time: f32,
}

impl Default for AnimController {
    fn default() -> Self {
        Self {
            state: Locomotion::Idle,
            clock: 0.0,
            prev: None,
            fade: 0.0,
            walk_at: 2.0,
            run_at: 8.0,
            hysteresis: 1.0,
            fade_time: 0.25,
        }
    }
}

impl AnimController {
    pub fn new(walk_at: f32, run_at: f32, fade_time: f32) -> Self {
        Self {
            walk_at,
            run_at,
            fade_time,
            ..Default::default()
        }
    }

    pub fn state(&self) -> Locomotion {
        self.state
    }

    /// 当前剪辑的相位（秒，已按剪辑长度折回）。
    pub fn clock(&self) -> f32 {
        self.clock
    }

    /// 直接切到某状态（事件/上层强制），走同样的淡入路径。
    pub fn force(&mut self, state: Locomotion) {
        if state == self.state {
            return;
        }
        self.prev = Some((self.state, self.clock));
        self.state = state;
        self.clock = 0.0;
        self.fade = self.fade_time;
    }

    fn target_state(&self, speed: f32) -> Locomotion {
        let hh = self.hysteresis * 0.5;
        match self.state {
            Locomotion::Idle => {
                if speed > self.run_at + hh {
                    Locomotion::Run
                } else if speed > self.walk_at + hh {
                    Locomotion::Walk
                } else {
                    Locomotion::Idle
                }
            }
            Locomotion::Walk => {
                if speed > self.run_at + hh {
                    Locomotion::Run
                } else if speed < self.walk_at - hh {
                    Locomotion::Idle
                } else {
                    Locomotion::Walk
                }
            }
            Locomotion::Run => {
                if speed < self.walk_at - hh {
                    Locomotion::Idle
                } else if speed < self.run_at - hh {
                    Locomotion::Walk
                } else {
                    Locomotion::Run
                }
            }
        }
    }

    /// 推进一帧（`dt` 秒）并产出关节面板。`speed` 是上层算好的速度标量。
    ///
    /// 输出长度 = [`ClipBank::joint_count`]；淡入期间取两段面板的逐矩阵混合。
    /// `dt <= 0` 或非有限时**相位不推进**，但仍返回当前面板（调用方拿到的
    /// 是上一帧的姿态，不是空）。
    pub fn sample(&mut self, dt: f32, speed: f32, bank: &ClipBank) -> Vec<[[f32; 4]; 4]> {
        let step = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };

        let want = self.target_state(speed);
        if want != self.state {
            self.prev = Some((self.state, self.clock));
            self.state = want;
            self.clock = 0.0;
            self.fade = self.fade_time;
        }

        self.clock += step;
        if let Some((_, t)) = self.prev.as_mut() {
            *t += step;
        }
        if self.fade > 0.0 {
            self.fade = (self.fade - step).max(0.0);
        }

        let n = bank.joint_count();
        let mut cur = vec![IDENTITY; n];
        let clip = bank.clip(self.state);
        clip.sample_into(self.clock, &mut cur);

        let Some((prev_state, prev_clock)) = self.prev else {
            return cur;
        };
        if self.fade <= 0.0 {
            self.prev = None;
            return cur;
        }
        let mut old = vec![IDENTITY; n];
        bank.clip(prev_state).sample_into(prev_clock, &mut old);
        let w = 1.0 - self.fade / self.fade_time.max(1e-6);
        for i in 0..n {
            cur[i] = blend_matrices(&old[i], &cur[i], w);
        }
        cur
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kp(t: f32, pos: [f32; 3]) -> Keyframe {
        Keyframe::at(t, pos)
    }

    /// 单关节、两帧（0 与 duration）平移剪辑。
    fn slide(duration: f32, from: [f32; 3], to: [f32; 3]) -> AnimationClip {
        AnimationClip::new(
            "slide",
            duration,
            vec![JointTrack {
                joint: 0,
                keys: vec![kp(0.0, from), kp(duration, to)],
            }],
        )
    }

    fn bank() -> ClipBank {
        ClipBank {
            idle: slide(1.0, [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]),
            walk: slide(1.0, [0.0, 1.0, 0.0], [0.0, 1.0, 0.0]),
            run: slide(1.0, [0.0, 2.0, 0.0], [0.0, 2.0, 0.0]),
        }
    }

    /// 平移的 y 分量（本组测试的剪辑都在 y 上滑）。
    fn ty(m: &[[f32; 4]; 4]) -> f32 {
        m[1][3]
    }

    #[test]
    fn identity_is_a_proper_affine_matrix() {
        assert_eq!(IDENTITY[0], [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(IDENTITY[3], [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn clip_interpolates_and_wraps() {
        let c = slide(1.0, [0.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        let mut p = Vec::new();
        c.sample_into(0.0, &mut p);
        assert!(ty(&p[0]).abs() < 1e-6, "t=0 应在起点");
        c.sample_into(0.5, &mut p);
        assert!(
            (ty(&p[0]) - 0.5).abs() < 1e-5,
            "中点应在 0.5 高度: {}",
            ty(&p[0])
        );
        // 循环：1.5 的相位与 0.5 相同（逐位一致，不是区间相等）。
        let mut q = Vec::new();
        c.sample_into(1.5, &mut q);
        assert_eq!(p, q, "循环相位必须与 0.5 完全相同");
        // 负时间同样折回：-0.5 等价 0.5。
        let mut r = Vec::new();
        c.sample_into(-0.5, &mut r);
        assert_eq!(p, r);
    }

    #[test]
    fn palette_grows_to_joint_index_and_defaults_identity() {
        let c = AnimationClip::new(
            "one",
            1.0,
            vec![JointTrack {
                joint: 2,
                keys: vec![kp(0.0, [0.0, 3.0, 0.0])],
            }],
        );
        let mut p = Vec::new();
        c.sample_into(0.2, &mut p);
        assert_eq!(p.len(), 3);
        assert_eq!(p[0], IDENTITY, "没有轨道的关节应是 bind 姿势");
        assert_eq!(p[1], IDENTITY);
        assert_eq!(ty(&p[2]), 3.0);
    }

    #[test]
    fn rotation_is_orthonormal_and_scale_hits_columns() {
        // 绕 z 轴 90°：q = (0,0,sin45,cos45)
        let half = std::f32::consts::FRAC_1_SQRT_2;
        let k = Keyframe::new(
            0.0,
            [5.0, 6.0, 7.0],
            [0.0, 0.0, half, half],
            [1.0, 1.0, 1.0],
        );
        let m = trs_matrix(&k);
        for (row, r) in m.iter().take(3).enumerate() {
            let n = (r[0] * r[0] + r[1] * r[1] + r[2] * r[2]).sqrt();
            assert!((n - 1.0).abs() < 1e-5, "第 {row} 行不是单位长度: {n}");
        }
        assert!((m[0][0] - 0.0).abs() < 1e-5, "90° 后 x 轴落到 y");
        assert!((m[0][1] + 1.0).abs() < 1e-5);
        assert_eq!(
            [m[0][3], m[1][3], m[2][3]],
            [5.0, 6.0, 7.0],
            "平移在最后一列"
        );

        let s = trs_matrix(&Keyframe::new(
            0.0,
            [0.0; 3],
            [0.0, 0.0, 0.0, 1.0],
            [2.0, 3.0, 4.0],
        ));
        assert_eq!([s[0][0], s[1][1], s[2][2]], [2.0, 3.0, 4.0], "缩放作用于列");
    }

    #[test]
    fn hysteresis_holds_the_state_inside_the_band() {
        let b = bank();
        let mut c = AnimController::new(2.0, 8.0, 0.2);
        assert_eq!(c.state(), Locomotion::Idle);
        c.sample(0.1, 2.4, &b);
        assert_eq!(c.state(), Locomotion::Idle, "带内不该切走");
        c.sample(0.1, 2.6, &b);
        assert_eq!(c.state(), Locomotion::Walk);
        c.sample(0.1, 1.6, &b);
        assert_eq!(c.state(), Locomotion::Walk, "回 Idle 要跌破 walk_at - h/2");
        c.sample(0.1, 1.4, &b);
        assert_eq!(c.state(), Locomotion::Idle);
        c.sample(0.1, 8.6, &b);
        assert_eq!(c.state(), Locomotion::Run);
        c.sample(0.1, 7.4, &b);
        assert_eq!(c.state(), Locomotion::Walk, "Run → Walk 在 run_at - h/2");
    }

    #[test]
    fn fade_blends_from_old_clip_to_new_one() {
        let b = bank();
        let mut c = AnimController::new(2.0, 8.0, 0.4);
        c.force(Locomotion::Walk);
        let p = c.sample(0.2, 3.0, &b);
        assert!(
            (ty(&p[0]) - 0.5).abs() < 1e-4,
            "半程应是 0 与 1 的平均: {}",
            ty(&p[0])
        );
        let p = c.sample(0.4, 3.0, &b);
        assert!(
            (ty(&p[0]) - 1.0).abs() < 1e-5,
            "淡入结束应到新剪辑: {}",
            ty(&p[0])
        );
        assert!(c.prev.is_none(), "淡入结束后清掉旧状态");
    }

    #[test]
    fn palette_length_covers_all_three_clips() {
        let b = ClipBank {
            idle: slide(1.0, [0.0; 3], [0.0; 3]),
            walk: AnimationClip::new(
                "walk",
                1.0,
                vec![JointTrack {
                    joint: 3,
                    keys: vec![kp(0.0, [0.0; 3])],
                }],
            ),
            run: slide(1.0, [1.0, 0.0, 0.0], [1.0, 0.0, 0.0]),
        };
        let mut c = AnimController::default();
        let p = c.sample(0.1, 0.0, &b);
        assert_eq!(p.len(), 4, "应长到最大关节数 4");
        assert_eq!(p[3], IDENTITY);
    }

    #[test]
    fn non_positive_dt_does_not_advance() {
        let b = bank();
        let mut c = AnimController::new(2.0, 8.0, 0.2);
        c.sample(0.5, 0.0, &b);
        let clock = c.clock();
        c.sample(0.0, 0.0, &b);
        assert!((c.clock() - clock).abs() < 1e-6);
        c.sample(f32::NAN, 0.0, &b);
        assert!((c.clock() - clock).abs() < 1e-6, "NaN 的 dt 不该推进相位");
        let p = c.sample(-1.0, 0.0, &b);
        assert_eq!(p.len(), 1, "负 dt 也要给出面板，长度为剪辑关节数");
    }

    #[test]
    fn same_inputs_give_bit_identical_palettes() {
        let b = bank();
        let mut a = AnimController::new(2.0, 8.0, 0.3);
        let mut d = AnimController::new(2.0, 8.0, 0.3);
        let script = [
            (0.1f32, 0.0f32),
            (0.1, 3.0),
            (0.1, 9.0),
            (0.1, 1.0),
            (0.1, 5.0),
        ];
        for (dt, sp) in script {
            let pa = a.sample(dt, sp, &b);
            let pb = d.sample(dt, sp, &b);
            assert_eq!(pa, pb, "同输入必须逐位一致");
        }
    }

    #[test]
    fn force_to_same_state_keeps_phase() {
        let b = bank();
        let mut c = AnimController::new(2.0, 8.0, 0.2);
        c.sample(0.4, 0.0, &b);
        let clock = c.clock();
        c.force(Locomotion::Idle);
        assert!((c.clock() - clock).abs() < 1e-6, "同状态强制不该重置相位");
        assert!(c.prev.is_none());
    }

    #[test]
    fn state_switch_restarts_phase() {
        let b = bank();
        let mut c = AnimController::new(2.0, 8.0, 0.2);
        c.sample(0.6, 0.0, &b);
        assert!(c.clock() > 0.5);
        c.sample(0.1, 3.0, &b);
        assert_eq!(c.state(), Locomotion::Walk);
        assert!(c.clock() < 0.15, "换状态后相位从零起: {}", c.clock());
    }
}
