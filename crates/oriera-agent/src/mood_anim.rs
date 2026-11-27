//! L9 情绪驱动动画(issue #203)——真实场景批次 WP7。
//!
//! **本文件当前是骨架占位**:模块声明与 manifest 条目已由「真实场景批次
//! 骨架」PR 立好,实现由 WP7 填充。
//!
//! 职责:认知层持续产出情绪([`crate::cognition::Mood`]),L6 刚有能播
//! 蒙皮剪辑的状态机(#208),中间"当前这个情绪该播哪条剪辑、什么时候切"
//! 是空白。本模块补这一层:Godot 的动画状态机靠人摆,OriEra 靠认知驱动。
//!
//! 契约(填充时必须保持)
//! - **默认编译的纯逻辑层**(本文件主体):剪辑组映射(情绪 → 剪辑名,
//!   缺剪辑回退 idle)+ 切换驱动的判定。纯 std、wasm32 可编译、零新增
//!   依赖、不依赖 L6;
//! - **需要 L6 的胶水**(把请求喂给 `oriera_subsys::anim_clip` 并采样)
//!   一律放在 `#[cfg(feature = "mood-anim")]` 门控的子模块里——默认构建
//!   不得引用 `oriera_subsys::anim*`;
//! - 确定性:同输入两次跑逐位一致;切换判定只看显式状态,不依赖墙钟;
//! - 回退诚实:没有映射 / 没有对应剪辑的情绪逐位回退 idle,不报错、
//!   不刷告警。

/// 骨架身份标记(实现落地后删除)。
pub const SCAFFOLD: &str = "oriera_agent::mood_anim";

#[cfg(test)]
mod tests {
    #[test]
    fn scaffold_is_wired() {
        assert_eq!(super::SCAFFOLD, "oriera_agent::mood_anim");
    }
}
