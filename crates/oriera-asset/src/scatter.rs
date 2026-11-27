//! L4 程序化散布:植被等实例的确定性放置——真实场景批次 WP5。
//!
//! **本文件当前是骨架占位**:模块声明与 manifest 条目已由「真实场景批次
//! 骨架」PR 立好,实现由 WP5 填充。
//!
//! 契约(填充时必须保持)
//! - 纯 std、wasm32 可编译,零新增依赖;
//! - 确定性:同种子同参数两次散布逐位一致(种子的推进方式沿用仓库的
//!   LCG 约定,不用 `DefaultHasher`);输出顺序只取决于输入;
//! - 地表高度经调用方注入的采样回调获取(典型来源是 `terrain`),本模块
//!   不直接依赖 `terrain` / `refs`;
//! - 坐标 y 向上;位置用 `[f32; 3]`。

/// 骨架身份标记(实现落地后删除)。
pub const SCAFFOLD: &str = "oriera_asset::scatter";

#[cfg(test)]
mod tests {
    #[test]
    fn scaffold_is_wired() {
        assert_eq!(super::SCAFFOLD, "oriera_asset::scatter");
    }
}
