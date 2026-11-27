//! L6 按名请求的剪辑状态机（feature `anim`）——真实场景批次 WP7。
//!
//! [`crate::anim::AnimController`] 是"速度三态"状态机（Idle / Walk / Run），
//! 状态只能由速度或 `force` 切换，没有"上层点播一段剪辑"的入口。情绪驱动
//! 动画（L9 `mood_anim`）需要的恰恰是点播：上层报一个剪辑名，状态机负责
//! 切换时机（最短驻留防抖）与交叉淡入。本模块补这一层——[`NamedClipController`]；
//! 采样与混合全部复用 `anim` 已有的 [`AnimationClip::sample_into`] 与
//! [`blend_matrices`]，不重写任何数值。
//!
//! **契约**
//! - 面板类型 `[[f32; 4]; 4]`（行主序，与 `anim` 一致）；长度 = 当前剪辑
//!   [`AnimationClip::joint_count`]，缺轨道的关节输出单位矩阵（bind 姿势，
//!   与 `sample_into` 同语义）。
//! - **相位延续**：剪辑间切换时全局相位 [`NamedClipController::clock`] 不
//!   归零，新剪辑按切换瞬间的相位继续播（`rem_euclid` 折回各自的循环长度）。
//!   这与 `AnimController` "切状态相位归零"不同——按名点播的剪辑多是同节奏
//!   循环动作，归零会在每次切换时跳回循环起点；从无剪辑到首次请求则相位
//!   归零（没有可延续的相位）。
//! - **切换判定只看显式状态**：防抖窗口（`min_dwell`）只比较当前剪辑的
//!   驻留计时，无墙钟、无随机；`request` 对未注册名返回 `false` 且**什么
//!   都不改**（诚实语义：点播失败就是失败，不静默兜底）。
//! - **确定性**：同输入两次跑面板逐位一致；剪辑表用 `BTreeMap` 有序存放，
//!   遍历序稳定。
//!
//! **诚实边界**
//! - 淡入沿用 [`blend_matrices`] 的**矩阵逐元素线性插值**——不是姿态空间
//!   混合，大角度旋转会缩水（`anim` 的既有简化，本模块不修复也不加重）。
//! - 无事件轨道、无 force / 打断优先级：上层想抢占就再 `request`（受
//!   `min_dwell` 约束）或重建控制器。
//! - 淡入期间面板长度只跟**当前**剪辑走：旧剪辑多出的关节被截断、少出的
//!   关节补单位矩阵，不做跨剪辑的最大并集。
//! - `min_dwell` 防抖没有排队：窗口内的请求被直接拒绝（返回 `false`），
//!   不缓存稍后重放——要不要重试是上层的策略。

use crate::anim::{blend_matrices, AnimationClip, IDENTITY};
use std::collections::BTreeMap;

/// 按名请求剪辑的通用状态机：上层点播，本机负责切换时机与交叉淡入。
///
/// 与 [`crate::anim::AnimController`] 的分工：后者是"速度三态"，状态只能由
/// 速度或 `force` 驱动；本机没有任何隐式状态来源——唯一的输入是
/// [`NamedClipController::request`] 的名字与每帧的 `dt`。
#[derive(Debug, Clone, PartialEq)]
pub struct NamedClipController {
    /// 已注册的剪辑。用 `BTreeMap` 是为了遍历序稳定（确定性先于性能：
    /// 点播按名查，树查找足够）。
    clips: BTreeMap<String, AnimationClip>,
    /// 当前剪辑名。不变式：`Some(name)` 时 `clips` 必含 `name`——`current`
    /// 只能由成功的 `request` 写入，而 `register` 只覆盖不删除。
    current: Option<String>,
    /// 淡入快照：旧剪辑名 + 切换瞬间的相位。淡入结束即清空。
    prev: Option<(String, f32)>,
    /// 全局相位（秒）：跨剪辑延续，见模块文档"相位延续"。
    clock: f32,
    /// 当前剪辑的驻留时长（秒）——防抖判定的唯一依据。
    dwell: f32,
    /// 剩余淡入时长（秒）；0 = 无淡入。
    fade: f32,
    /// 淡入总时长（秒）；≤ 0 视为硬切。
    fade_time: f32,
    /// 切换防抖窗口（秒）；当前剪辑驻留不满它时新请求被拒绝。
    min_dwell: f32,
}

impl NamedClipController {
    /// `fade_time` 是交叉淡入时长；≤ 0 视作硬切（切换无过渡）。
    pub fn new(fade_time: f32) -> Self {
        Self {
            clips: BTreeMap::new(),
            current: None,
            prev: None,
            clock: 0.0,
            dwell: 0.0,
            fade: 0.0,
            fade_time,
            min_dwell: 0.0,
        }
    }

    /// 注册（或覆盖同名）一段剪辑。覆盖正在播放的名字：下一次
    /// [`NamedClipController::sample`] 起按新数据采样，当前名与相位不变。
    pub fn register(&mut self, name: &str, clip: AnimationClip) {
        self.clips.insert(name.to_string(), clip);
    }

    /// 设置切换防抖窗口（秒）；负值夹到 0（无防抖）。防抖没有排队：窗口
    /// 内的请求被直接拒绝，要不要稍后重试是上层的策略。
    pub fn min_dwell(&mut self, seconds: f32) {
        self.min_dwell = seconds.max(0.0);
    }

    /// 点播一段剪辑。判定顺序（诚实语义，逐条短路）：
    /// 1. 未注册名 → 返回 `false`，**什么都不改**（不兜底、不告警）；
    /// 2. 与当前同名 → 返回 `true`，幂等（相位与驻留都不动）；
    /// 3. 当前剪辑驻留不满 `min_dwell` → 返回 `false`（防抖窗口内丢弃）；
    /// 4. 否则切换：旧剪辑按当前相位留快照、新剪辑相位延续（`clock` 不
    ///    归零）、驻留清零，返回 `true`。首次请求（此前无剪辑）相位归零、
    ///    无淡入对象。
    pub fn request(&mut self, name: &str) -> bool {
        if !self.clips.contains_key(name) {
            return false;
        }
        match &self.current {
            Some(cur) if cur == name => true,
            Some(_) if self.dwell < self.min_dwell => false,
            _ => {
                let had = self.current.is_some();
                self.prev = self.current.take().map(|old| (old, self.clock));
                if !had {
                    self.clock = 0.0;
                }
                self.fade = if had { self.fade_time } else { 0.0 };
                self.current = Some(name.to_string());
                self.dwell = 0.0;
                true
            }
        }
    }

    /// 当前剪辑名；从未成功请求过时为 `None`。
    pub fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// 全局相位（秒）。跨剪辑切换延续、首次请求归零——语义见
    /// [`NamedClipController::request`] 与模块文档。
    pub fn clock(&self) -> f32 {
        self.clock
    }

    /// 推进一帧（`dt` 秒）并产出当前剪辑的关节面板。
    ///
    /// - 长度 = 当前剪辑 [`AnimationClip::joint_count`]（对齐 `sample_into`
    ///   语义：缺轨道的关节补单位矩阵 bind 姿势）。从未成功请求过任何剪辑
    ///   时返回空 `Vec`。
    /// - 淡入期间按 [`blend_matrices`] 逐矩阵混合旧快照与当前姿态，权重随
    ///   时间从 0 走到 1；淡入结束输出与"从头播当前剪辑"逐位一致。
    /// - `dt <= 0` 或非有限：相位与驻留都不推进，但仍返回当前面板（调用方
    ///   拿到的是上一帧的姿态，不是空）。
    pub fn sample(&mut self, dt: f32) -> Vec<[[f32; 4]; 4]> {
        let step = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
        self.clock += step;
        self.dwell += step;
        if let Some((_, t)) = self.prev.as_mut() {
            *t += step;
        }
        if self.fade > 0.0 {
            self.fade = (self.fade - step).max(0.0);
        }

        let Some(name) = self.current.clone() else {
            return Vec::new();
        };
        let clip = self.clips.get(&name).expect("current 剪辑必已注册");
        let n = clip.joint_count();
        let mut cur = vec![IDENTITY; n];
        clip.sample_into(self.clock, &mut cur);

        // 淡入结束（或从未淡入）：快照清掉，之后输出与"从头播当前剪辑"
        // 逐位一致。
        if self.fade <= 0.0 {
            self.prev = None;
            return cur;
        }
        // 首次请求不设淡入快照（fade = 0，上面已返回）；这里兜底防御。
        let Some((prev_name, prev_clock)) = self.prev.as_ref() else {
            return cur;
        };
        let prev_clip = self.clips.get(prev_name).expect("prev 剪辑必已注册");
        let mut old = vec![IDENTITY; n];
        prev_clip.sample_into(*prev_clock, &mut old);
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
    use crate::anim::{JointTrack, Keyframe};

    /// 平移的 y 分量（本组测试的剪辑都在 y 上动）。
    fn ty(m: &[[f32; 4]; 4]) -> f32 {
        m[1][3]
    }

    /// 静态单关节剪辑：`duration = 0`（静态，恒取首帧），恒为 (0, y, 0)。
    fn static_y(name: &str, y: f32) -> AnimationClip {
        AnimationClip::new(
            name,
            0.0,
            vec![JointTrack {
                joint: 0,
                keys: vec![Keyframe::at(0.0, [0.0, y, 0.0])],
            }],
        )
    }

    /// 单关节 y 平移循环（0 → 1，时长 `duration`）。
    fn loop_y(name: &str, duration: f32) -> AnimationClip {
        AnimationClip::new(
            name,
            duration,
            vec![JointTrack {
                joint: 0,
                keys: vec![
                    Keyframe::at(0.0, [0.0, 0.0, 0.0]),
                    Keyframe::at(duration, [0.0, 1.0, 0.0]),
                ],
            }],
        )
    }

    #[test]
    fn register_request_and_current_round_trip() {
        let mut c = NamedClipController::new(0.25);
        assert_eq!(c.current(), None);
        assert_eq!(c.clock(), 0.0);
        c.register("idle", static_y("idle", 0.0));
        c.register("wave", static_y("wave", 1.0));
        assert!(c.request("wave"), "已注册名应被接受");
        assert_eq!(c.current(), Some("wave"));
        assert!(c.request("idle"));
        assert_eq!(c.current(), Some("idle"));
        // 重复注册同名覆盖：当前正在播的剪辑被替换后按新数据采样。
        // 刚切到 idle 时 fade 刚起（w=0），此刻面板还是旧姿态——淡入语义
        // 如此；推进越过 fade_time 后覆盖生效。
        c.register("idle", static_y("idle", 5.0));
        let p = c.sample(0.0);
        assert!(
            (ty(&p[0]) - 1.0).abs() < 1e-5,
            "fade 起点（w=0）应仍是旧姿态"
        );
        let p = c.sample(0.5);
        assert!((ty(&p[0]) - 5.0).abs() < 1e-6, "越过 fade 后按新数据采样");
        assert_eq!(c.current(), Some("idle"), "覆盖不改当前名");
    }

    #[test]
    fn unknown_name_is_ignored_and_returns_false() {
        let mut c = NamedClipController::new(0.2);
        assert!(!c.request("ghost"), "空表上未知名也是 false");
        assert!(c.current().is_none());
        assert!(c.sample(0.1).is_empty(), "从未成功请求过 → 无面板");

        c.register("idle", static_y("idle", 0.0));
        assert!(c.request("idle"));
        c.sample(0.3);
        let before = c.sample(0.1);
        assert!(!c.request("wave"), "未注册名被忽略");
        assert_eq!(c.current(), Some("idle"), "被拒绝的请求不改当前名");
        let after = c.sample(0.1);
        assert_eq!(before, after, "被忽略的请求不改面板");
    }

    #[test]
    fn fade_blends_then_lands_bit_exact_on_the_new_clip() {
        let mut c = NamedClipController::new(1.0);
        c.register("low", static_y("low", 0.0));
        c.register("high", static_y("high", 1.0));
        assert!(c.request("low"));
        let p = c.sample(0.25);
        assert!(ty(&p[0]).abs() < 1e-6, "首次请求无淡入对象，直接是低姿态");

        assert!(c.request("high"));
        let p = c.sample(0.5);
        assert!(
            (ty(&p[0]) - 0.5).abs() < 1e-5,
            "半程应混合 0 与 1: {}",
            ty(&p[0])
        );
        assert!(ty(&p[0]) != 1.0, "淡入期间不能已经是终态");
        let p = c.sample(0.5);
        assert!(
            (ty(&p[0]) - 1.0).abs() < 1e-5,
            "淡入结束应到新剪辑: {}",
            ty(&p[0])
        );

        // 终态与"从一开始就播 high"逐位一致。
        let mut c2 = NamedClipController::new(1.0);
        c2.register("high", static_y("high", 1.0));
        assert!(c2.request("high"));
        let q = c2.sample(1.25);
        assert_eq!(p, q, "稳态必须逐位一致");
    }

    #[test]
    fn min_dwell_blocks_requests_inside_the_window() {
        let mut c = NamedClipController::new(0.0);
        c.register("a", static_y("a", 0.0));
        c.register("b", static_y("b", 1.0));
        c.min_dwell(1.0);
        assert!(c.request("a"), "首次请求不受防抖约束");
        c.sample(0.4); // dwell = 0.4
        assert!(c.request("a"), "同名请求幂等，窗口内也返回 true");
        assert!(!c.request("b"), "驻留 0.4 < min_dwell 1.0 → 拒绝");
        assert_eq!(c.current(), Some("a"));
        c.sample(0.61); // dwell = 1.01
        assert!(c.request("b"), "驻留满 → 放行");
        assert_eq!(c.current(), Some("b"));
    }

    #[test]
    fn phase_continues_across_switch() {
        let mut c = NamedClipController::new(0.0); // 硬切，只看相位
        c.register("a", loop_y("a", 1.0));
        c.register("b", loop_y("b", 1.0));
        assert!(c.request("a"));
        let p = c.sample(0.6);
        assert!((ty(&p[0]) - 0.6).abs() < 1e-5, "a 在相位 0.6 应 y=0.6");
        assert!(c.request("b"));
        let p = c.sample(0.0); // dt=0 不推进，看切换瞬间的面板
        assert!(
            (ty(&p[0]) - 0.6).abs() < 1e-5,
            "相位延续：b 应从 0.6 继续而不是跳回循环起点: {}",
            ty(&p[0])
        );
    }

    #[test]
    fn palette_length_follows_current_clip_and_pads_identity() {
        let mut c = NamedClipController::new(0.0);
        let wide = AnimationClip::new(
            "wide",
            0.0,
            vec![
                JointTrack {
                    joint: 0,
                    keys: vec![Keyframe::at(0.0, [1.0, 0.0, 0.0])],
                },
                JointTrack {
                    joint: 2,
                    keys: vec![Keyframe::at(0.0, [0.0, 0.0, 3.0])],
                },
            ],
        );
        c.register("wide", wide);
        c.register("narrow", static_y("narrow", 0.5));
        assert!(c.request("wide"));
        let p = c.sample(0.1);
        assert_eq!(p.len(), 3, "长度 = 当前剪辑 joint_count");
        assert_eq!(p[1], IDENTITY, "缺轨道的关节补 bind 姿势");
        assert!((p[2][2][3] - 3.0).abs() < 1e-6);
        assert!(c.request("narrow"));
        let p = c.sample(0.1);
        assert_eq!(p.len(), 1, "切到小剪辑后面板收缩");
        assert!((ty(&p[0]) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn non_positive_dt_does_not_advance_but_still_returns_palette() {
        let mut c = NamedClipController::new(0.2);
        c.register("a", loop_y("a", 1.0));
        assert!(c.request("a"));
        c.sample(0.5);
        let clock = c.clock();
        let p = c.sample(0.0);
        assert!((c.clock() - clock).abs() < 1e-6, "dt=0 不推进相位");
        assert_eq!(p.len(), 1, "坏 dt 也要给出面板");
        c.sample(f32::NAN);
        assert!((c.clock() - clock).abs() < 1e-6, "NaN 不推进");
        c.sample(-1.0);
        assert!((c.clock() - clock).abs() < 1e-6);
        let p = c.sample(-1.0);
        assert_eq!(p.len(), 1, "负 dt 仍返回当前面板");
    }

    #[test]
    fn same_inputs_give_bit_identical_palettes() {
        fn script(c: &mut NamedClipController) -> Vec<Vec<[[f32; 4]; 4]>> {
            c.register("a", loop_y("a", 1.0));
            c.register("b", static_y("b", 2.0));
            c.min_dwell(0.5);
            let mut out = Vec::new();
            assert!(c.request("a"));
            out.push(c.sample(0.3));
            out.push(c.sample(0.3));
            out.push(c.sample(0.3)); // 驻留 0.9 ≥ 0.5
            assert!(c.request("b"));
            out.push(c.sample(0.25));
            out.push(c.sample(0.25));
            out.push(c.sample(0.25)); // fade（0.6）结束
            assert!(!c.request("ghost"));
            out.push(c.sample(0.1));
            out
        }
        let pa = script(&mut NamedClipController::new(0.6));
        let pb = script(&mut NamedClipController::new(0.6));
        assert_eq!(pa, pb, "同输入必须逐位一致");
    }
}
