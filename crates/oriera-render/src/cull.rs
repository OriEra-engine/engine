//! L5 可见性剔除与 LOD(feature `cull`,默认关)——真实场景批次 WP3。
//!
//! **本文件当前是骨架占位**:feature 位、模块声明、CI 腿(`render-scene`)
//! 与 manifest 条目已由「真实场景批次骨架」PR 立好,实现由 WP3 填充。
//!
//! 契约(填充时必须保持)
//! - 纯 CPU、headless 可测,零新增依赖;
//! - 矩阵一律 [`crate::camera::Mat4`](行主序、列向量),向量用 `[f32; 3]`;
//! - 输入输出只经过 [`crate::scene`] 词汇:吃场景实例,产可见实例
//!   (及其选中的 LOD 档位),供合批器消费;
//! - 不 `use` 同批次其它模块(`shadow` / `ibl` / `instancing` / `sky`);
//! - 确定性:无随机、无哈希迭代序依赖,同输入两次跑逐位一致。

/// 骨架身份标记(实现落地后删除)。
pub const SCAFFOLD: &str = "oriera_render::cull";

#[cfg(test)]
mod tests {
    #[test]
    fn scaffold_is_wired() {
        assert_eq!(super::SCAFFOLD, "oriera_render::cull");
    }
}
