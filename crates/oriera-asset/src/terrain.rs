//! L4 程序化地形:高度场与网格构建——真实场景批次 WP5。
//!
//! **本文件当前是骨架占位**:模块声明与 manifest 条目已由「真实场景批次
//! 骨架」PR 立好,实现由 WP5 填充。
//!
//! 契约(填充时必须保持)
//! - 纯 std、wasm32 可编译,零新增依赖;
//! - 确定性:同种子同参数两次生成逐位一致;噪声与网格生成不依赖浮点
//!   求和顺序以外的隐藏状态;
//! - 坐标 y 向上;位置用 `[f32; 3]`,与 L5 `camera` / `scene` 词汇对齐;
//! - 不 `use` 同批次其它模块(`refs` / `scatter`):散布模块通过公开的
//!   高度采样接口取地表高度,不读本模块的内部结构。

/// 骨架身份标记(实现落地后删除)。
pub const SCAFFOLD: &str = "oriera_asset::terrain";

#[cfg(test)]
mod tests {
    #[test]
    fn scaffold_is_wired() {
        assert_eq!(super::SCAFFOLD, "oriera_asset::terrain");
    }
}
