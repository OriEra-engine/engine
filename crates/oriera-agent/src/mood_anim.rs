//! L9 情绪驱动的剪辑选择（默认编译纯逻辑）——真实场景批次 WP7。
//!
//! 认知层每轮产出情绪（[`Mood`]，五值），L6 的按名请求剪辑状态机（#208，
//! `oriera_subsys::anim_clip::NamedClipController`）能点播具名剪辑并处理
//! 交叉淡入。中间"当前这个情绪该播哪条剪辑、什么时候切"由本模块补上：
//! Godot 的动画状态机靠人摆节点连线，OriEra 靠认知驱动。
//!
//! **分层**
//! - **纯逻辑层（本文件主体，默认编译）**：[`MoodClipTable`]（情绪 → 剪辑
//!   偏好序，序即 fallback 链）与 [`MoodAnimDriver`]（驻留防抖 + 组切换）。
//!   纯 std、wasm32 可编译、零新增依赖，**不引用** `oriera_subsys`；
//! - **胶水层（feature `mood-anim`）**：[`glue`] 把 driver 的选择喂给 L6
//!   状态机。存在理由：`oriera_subsys` 的 `anim` / `anim_clip` 是 feature
//!   门控模块，默认构建里根本不存在——而情绪映射必须在没有 L6 的腿（wasm
//!   预算、纯认知测试）也能编译运行。所以"接线"被隔离在本模块，只有显式
//!   开 `mood-anim`（透传 `oriera-subsys/anim`）的构建才看得见它。
//!
//! **契约**
//! - [`MoodAnimDriver::update`] 返回"当前应请求的剪辑名"：情绪驻留满
//!   `min_dwell` 才切组；查无此组 / 空组逐位回退 [`IDLE_CLIP`]；同情绪不切。
//!   防抖判定只看显式驻留计时，无墙钟、无随机，双跑逐位一致。
//! - 偏好序（fallback 链）的完整消费（链头点播失败就沿链试下一个）由上层
//!   用 [`MoodClipTable::chain`] + `glue::apply` 完成——driver 只取链头，
//!   不感知 L6 注册表。
//!
//! **诚实边界**
//! - 无情绪强度 / 混合权重：情绪是离散五值，切换就是换组，不做"开心 70%
//!   + 悲伤 30%"的加权。
//! - 无事件轨道、无打断优先级；`min_dwell` 窗口内的情绪变化被直接忽略
//!   （不排队、不合并）。
//! - 回退是逐位回退 idle：不报错、不刷告警——缺配置是配置问题，不是
//!   运行时错误。

use crate::cognition::Mood;

/// 回退剪辑名：查无此组 / 空组时 [`MoodAnimDriver::update`] 返回的名字。
pub const IDLE_CLIP: &str = "idle";

/// 五情绪的固定槽位数。`Mood` 没有 `Ord` / `Hash`（`cognition.rs` 的公共
/// 契约冻结，不能为做映射键加 derive）——用 `match` 分派到数组槽位，
/// 存储与遍历顺序天然稳定。
const MOOD_SLOTS: usize = 5;

fn slot(mood: Mood) -> usize {
    match mood {
        Mood::Happy => 0,
        Mood::Neutral => 1,
        Mood::Sad => 2,
        Mood::Excited => 3,
        Mood::Thoughtful => 4,
    }
}

/// 五情绪各自的剪辑偏好序（纯逻辑，不含任何 L6 类型）。
///
/// 每组是一个 `Vec<String>`，**序即 fallback 链**：链头是最想播的剪辑，
/// 上层点播失败（L6 未注册该名）就沿链退到下一个——完整消费由上层用
/// [`MoodClipTable::chain`] 与 `glue::apply` 完成，本类型不感知 L6。
/// 未配置的情绪组由 [`MoodAnimDriver`] 回退 [`IDLE_CLIP`]。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MoodClipTable {
    groups: [Option<Vec<String>>; MOOD_SLOTS],
}

impl MoodClipTable {
    /// 空表：五组全部未配置，驱动器永远回退 idle。
    pub fn new() -> Self {
        Self::default()
    }

    /// 配置一个情绪组的偏好序（链头在前）。重复配置同一情绪覆盖旧组。
    pub fn with_group(mut self, mood: Mood, clips: Vec<String>) -> Self {
        self.groups[slot(mood)] = Some(clips);
        self
    }

    /// 某情绪的偏好序；未配置返回 `None`。
    pub fn chain(&self, mood: Mood) -> Option<&[String]> {
        self.groups[slot(mood)].as_deref()
    }
}

/// 情绪 → 剪辑请求的驱动器（纯逻辑）。
///
/// 显式状态只有当前情绪与驻留计时：[`MoodAnimDriver::update`] 每帧推进
/// 驻留，情绪变化但驻留不满 `min_dwell` 时维持旧组——认知层的情绪抖动
/// 不会立即打到动画上。判定无墙钟、无随机，双跑逐位一致。
#[derive(Debug, Clone, PartialEq)]
pub struct MoodAnimDriver {
    table: MoodClipTable,
    min_dwell: f32,
    active: Option<Mood>,
    dwell: f32,
}

impl MoodAnimDriver {
    /// 空表 + `min_dwell` 防抖窗口（秒，负值夹到 0）。空表驱动器永远
    /// 请求 idle（查无此组的诚实回退）——配好 [`MoodClipTable`] 再用。
    pub fn new(table: MoodClipTable, min_dwell: f32) -> Self {
        Self {
            table,
            min_dwell: min_dwell.max(0.0),
            active: None,
            dwell: 0.0,
        }
    }

    /// 当前生效的情绪组；从未被驱动过时为 `None`。
    pub fn active_mood(&self) -> Option<Mood> {
        self.active
    }

    /// 推进一帧（`dt` 秒）并返回"当前应请求的剪辑名"。
    ///
    /// - 首帧（尚无生效情绪）直接接受 `mood`，无防抖对象；
    /// - `mood` 与当前不同且驻留满 `min_dwell` → 切组、驻留清零；
    /// - 其余情况维持现状（同情绪不切，也不清驻留计时）；
    /// - 返回值：生效组的链头；查无此组 / 空组 → [`IDLE_CLIP`]；
    /// - `dt <= 0` 或非有限：驻留不推进，但仍返回当前决策（调用方拿到
    ///   的是有效名字，不是空）。
    pub fn update(&mut self, mood: Mood, dt: f32) -> &str {
        let step = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
        self.dwell += step;
        match self.active {
            None => {
                self.active = Some(mood);
                self.dwell = 0.0;
            }
            Some(cur) if cur != mood && self.dwell >= self.min_dwell => {
                self.active = Some(mood);
                self.dwell = 0.0;
            }
            _ => {}
        }
        self.request_name()
    }

    /// 生效组的链头；缺组 / 空组 / 尚未生效 → [`IDLE_CLIP`]。
    fn request_name(&self) -> &str {
        let Some(mood) = self.active else {
            return IDLE_CLIP;
        };
        match self.table.chain(mood) {
            Some(chain) => chain.first().map(String::as_str).unwrap_or(IDLE_CLIP),
            None => IDLE_CLIP,
        }
    }
}

/// L6 接线胶水（feature `mood-anim`）。
///
/// **存在理由**：`oriera_subsys` 的 `anim` / `anim_clip` 是 feature 门控
/// 模块，默认构建不得引用 `oriera_subsys::anim*`——而情绪映射（本文件
/// 主体）必须在没有 L6 的腿也能编译。所以"把请求喂给 L6"这一步被隔离在
/// 本模块，只有显式开 `mood-anim`（透传 `oriera-subsys/anim`）的构建才
/// 看得见它。薄胶水：只转发，不加任何策略。
#[cfg(feature = "mood-anim")]
pub mod glue {
    /// 把一条剪辑请求转发给 L6 的
    /// `oriera_subsys::anim_clip::NamedClipController`。
    ///
    /// 返回值原样透传 `request` 的诚实语义：未注册名 → `false` 且状态
    /// 不变；成功切换 / 幂等同名 → `true`。防抖与淡入都由 controller 负责。
    pub fn apply(
        controller: &mut oriera_subsys::anim_clip::NamedClipController,
        request: &str,
    ) -> bool {
        controller.request(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Happy → [cheer, wave]；Sad → [slump]；其余三组未配置。
    fn table() -> MoodClipTable {
        MoodClipTable::new()
            .with_group(Mood::Happy, vec!["cheer".into(), "wave".into()])
            .with_group(Mood::Sad, vec!["slump".into()])
    }

    #[test]
    fn chains_expose_groups_in_preference_order() {
        let t = table();
        assert_eq!(
            t.chain(Mood::Happy).unwrap(),
            ["cheer", "wave"],
            "序即 fallback 链"
        );
        assert_eq!(t.chain(Mood::Sad).unwrap(), ["slump"]);
        assert!(t.chain(Mood::Neutral).is_none());
        assert!(t.chain(Mood::Excited).is_none());
        assert!(t.chain(Mood::Thoughtful).is_none());
        // 覆盖语义：重复配置同一情绪替换旧组。
        let t = table().with_group(Mood::Happy, vec!["bounce".into()]);
        assert_eq!(t.chain(Mood::Happy).unwrap(), ["bounce"]);
    }

    #[test]
    fn driver_returns_chain_head_for_every_mood() {
        let mut d = MoodAnimDriver::new(table(), 0.0);
        assert_eq!(d.active_mood(), None);
        assert_eq!(d.update(Mood::Happy, 0.016), "cheer");
        assert_eq!(d.update(Mood::Sad, 0.016), "slump");
        assert_eq!(d.active_mood(), Some(Mood::Sad));
        // 未配置的三种情绪逐位回退 idle。
        assert_eq!(d.update(Mood::Excited, 0.016), IDLE_CLIP);
        assert_eq!(d.update(Mood::Thoughtful, 0.016), IDLE_CLIP);
        assert_eq!(d.update(Mood::Neutral, 0.016), IDLE_CLIP);
    }

    #[test]
    fn missing_and_empty_groups_fall_back_to_idle() {
        let t = MoodClipTable::new()
            .with_group(Mood::Neutral, Vec::new()) // 空组
            .with_group(Mood::Sad, vec!["slump".into()]);
        let mut d = MoodAnimDriver::new(t, 0.0);
        assert_eq!(d.update(Mood::Sad, 0.01), "slump");
        assert_eq!(d.update(Mood::Neutral, 0.01), IDLE_CLIP, "空组 → idle");
        assert_eq!(d.update(Mood::Happy, 0.01), IDLE_CLIP, "缺组 → idle");
        // 空表驱动器永远 idle。
        let mut e = MoodAnimDriver::new(MoodClipTable::new(), 0.0);
        assert_eq!(e.update(Mood::Happy, 0.01), IDLE_CLIP);
        assert_eq!(e.update(Mood::Excited, 0.01), IDLE_CLIP);
    }

    #[test]
    fn dwell_blocks_mood_flapping() {
        let mut d = MoodAnimDriver::new(table(), 1.0);
        assert_eq!(d.update(Mood::Happy, 0.4), "cheer", "首帧直接接受");
        assert_eq!(d.active_mood(), Some(Mood::Happy));
        assert_eq!(
            d.update(Mood::Sad, 0.4),
            "cheer",
            "驻留 0.4 < 1.0 → 维持旧组"
        );
        assert_eq!(d.active_mood(), Some(Mood::Happy));
        assert_eq!(d.update(Mood::Sad, 0.7), "slump", "驻留 1.1 ≥ 1.0 → 放行");
        assert_eq!(d.active_mood(), Some(Mood::Sad));
    }

    #[test]
    fn same_mood_never_switches_or_resets_dwell() {
        let mut d = MoodAnimDriver::new(table(), 1.0);
        assert_eq!(d.update(Mood::Happy, 0.5), "cheer");
        assert_eq!(
            d.update(Mood::Happy, 0.5),
            "cheer",
            "同情绪不切、不清驻留（累计到 1.0）"
        );
        assert_eq!(
            d.update(Mood::Happy, 0.5),
            "cheer",
            "驻留 1.5，同情绪继续累计"
        );
        assert_eq!(d.update(Mood::Sad, 0.1), "slump", "驻留 1.6 ≥ 1.0 → 立即切");
        assert_eq!(
            d.update(Mood::Happy, 0.2),
            "slump",
            "切组后驻留从零起，0.2 被挡"
        );
    }

    #[test]
    fn non_positive_dt_does_not_advance_dwell() {
        let mut d = MoodAnimDriver::new(table(), 1.0);
        assert_eq!(d.update(Mood::Happy, 0.5), "cheer"); // 首帧接受，驻留清零
        assert_eq!(
            d.update(Mood::Sad, 0.0),
            "cheer",
            "dt=0 驻留不动（0.0 < 1.0）"
        );
        assert_eq!(d.update(Mood::Sad, f32::NAN), "cheer", "NaN 同理");
        assert_eq!(d.update(Mood::Sad, -1.0), "cheer");
        assert_eq!(d.update(Mood::Sad, 0.5), "cheer", "0.5 < 1.0 仍挡");
        assert_eq!(d.update(Mood::Sad, 0.6), "slump", "0.5+0.6=1.1 ≥ 1.0 放行");
    }

    #[test]
    fn same_inputs_give_bit_identical_decision_streams() {
        fn script() -> Vec<(Mood, f32)> {
            vec![
                (Mood::Happy, 0.3),      // 首帧接受，驻留清零
                (Mood::Happy, 0.3),      // 驻留 0.3，同情绪
                (Mood::Happy, 0.8),      // 驻留 1.1，同情绪继续累计
                (Mood::Excited, 0.1),    // 驻留 1.2 放行 → 缺组 → idle
                (Mood::Sad, 0.05),       // 刚切完被挡（0.05）→ 仍 idle
                (Mood::Sad, 1.0),        // 驻留 1.05 → 切 Sad
                (Mood::Sad, 0.1),        // 同情绪不切
                (Mood::Happy, 0.05),     // 被挡（0.15）
                (Mood::Thoughtful, 0.9), // 驻留 1.05 → 缺组 → idle
            ]
        }
        let mut a = MoodAnimDriver::new(table(), 1.0);
        let mut b = MoodAnimDriver::new(table(), 1.0);
        for (mood, dt) in script() {
            let na = a.update(mood, dt);
            let nb = b.update(mood, dt);
            assert_eq!(na, nb, "同输入必须逐位一致");
        }
        assert_eq!(a.active_mood(), b.active_mood());
    }
}

#[cfg(all(test, feature = "mood-anim"))]
mod glue_tests {
    use super::glue::apply;
    use super::{MoodAnimDriver, MoodClipTable, IDLE_CLIP};
    use crate::cognition::Mood;
    use oriera_subsys::anim::{AnimationClip, JointTrack, Keyframe};
    use oriera_subsys::anim_clip::NamedClipController;

    /// 静态单关节剪辑（duration = 0，恒取首帧）。
    fn clip(name: &str, y: f32) -> AnimationClip {
        AnimationClip::new(
            name,
            0.0,
            vec![JointTrack {
                joint: 0,
                keys: vec![Keyframe::at(0.0, [0.0, y, 0.0])],
            }],
        )
    }

    /// apply 的转发语义：已知名 true + 状态落位、未知名 false + 状态不变，
    /// 以及"driver 决策 → glue 转发 → controller 状态"的小回路。
    #[test]
    fn apply_forwards_request_semantics() {
        let mut c = NamedClipController::new(0.1);
        assert!(!apply(&mut c, IDLE_CLIP), "未注册名：apply 原样返回 false");
        assert!(c.current().is_none(), "被拒绝的请求不改状态");

        c.register(IDLE_CLIP, clip("idle", 0.0));
        c.register("slump", clip("slump", 1.0));
        assert!(apply(&mut c, IDLE_CLIP));
        assert_eq!(c.current(), Some(IDLE_CLIP));
        assert!(!apply(&mut c, "ghost"), "未知名语义原样透传");
        assert_eq!(c.current(), Some(IDLE_CLIP));

        // driver + glue 的小回路：认知情绪 → 剪辑名 → controller。
        let mut d = MoodAnimDriver::new(
            MoodClipTable::new().with_group(Mood::Sad, vec!["slump".into()]),
            0.0,
        );
        let name = d.update(Mood::Sad, 0.016);
        assert!(apply(&mut c, name));
        assert_eq!(c.current(), Some("slump"));
    }
}
