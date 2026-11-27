//! L4 资产引用图(issue #206)——真实场景批次 WP9。
//!
//! **本文件当前是骨架占位**:模块声明与 manifest 条目已由「真实场景批次
//! 骨架」PR 立好,实现由 WP9 填充。
//!
//! 职责:回答"这个世界需要哪些资产、缺不缺"。现状是分享与发行靠各条线
//! 临时手捆,L4 自己没有可机读的资产需求表。
//!
//! 契约(填充时必须保持)
//! - 纯 std、wasm32 可编译(不 `std::fs`——磁盘由调用方注入),零新增依赖;
//! - 确定性:预检报告(缺失 / 冗余清单)的顺序只取决于输入,同输入逐位一致;
//! - 纯加法:不带资产需求表的世界,行为与现版本逐位一致;
//! - 需要给 [`crate::AssetStore`] 记录来源时只做加法,既有公共签名不变。

/// 骨架身份标记(实现落地后删除)。
pub const SCAFFOLD: &str = "oriera_asset::refs";

#[cfg(test)]
mod tests {
    #[test]
    fn scaffold_is_wired() {
        assert_eq!(super::SCAFFOLD, "oriera_asset::refs");
    }
}
