//! L6 通用剪辑状态机(feature `anim`)——真实场景批次 WP7。
//!
//! **本文件当前是骨架占位**:模块声明与 manifest 条目已由「真实场景批次
//! 骨架」PR 立好,实现由 WP7 填充。
//!
//! 职责:[`crate::anim::AnimController`] 是"速度三态"状态机(Idle / Walk /
//! Run),状态只能由速度或 [`crate::anim::AnimController::force`] 切换,
//! 没有按名请求剪辑的入口。情绪驱动动画需要的是"上层请求一个具名剪辑,
//! 状态机负责切换时机与交叉淡入"——本模块补这一层,采样与混合复用
//! `anim` 已有的 [`crate::anim::AnimationClip`] 与 [`crate::anim::blend_matrices`]。
//!
//! 契约(填充时必须保持)
//! - 纯 CPU、headless 可测,零新增依赖;
//! - **不改** `anim.rs` 里既有类型的公共契约(`AnimController` /
//!   `ClipBank` / `Locomotion` 的语义与签名冻结);
//! - 面板类型沿用 `[[f32; 4]; 4]`(行主序,与 `anim` 一致);
//! - 确定性:同输入两次跑面板逐位一致;切换判定(迟滞 / 最短驻留)只看
//!   显式状态,不依赖墙钟。

/// 骨架身份标记(实现落地后删除)。
pub const SCAFFOLD: &str = "oriera_subsys::anim_clip";

#[cfg(test)]
mod tests {
    #[test]
    fn scaffold_is_wired() {
        assert_eq!(super::SCAFFOLD, "oriera_subsys::anim_clip");
    }
}
