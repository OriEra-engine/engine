//! L6 空间音频:纯几何层(feature `audio`,默认关)——真实场景批次 WP8。
//!
//! **本文件当前是骨架占位**:feature 位、模块声明、CI 腿
//! (`subsys-nav-anim-audio`)与 manifest 条目已由「真实场景批次骨架」PR
//! 立好,实现由 WP8 填充。
//!
//! 职责:回答"听者在这里,声源在那里,每个声源该以多大增益、偏到哪一侧
//! 播出来"。只做确定性几何计算,输出给混音器的增益与声像;音频设备、
//! 解码、混音、混响都不在引擎内。
//!
//! 契约(填充时必须保持)
//! - 纯 CPU、headless 可测,零新增依赖;
//! - 坐标 y 向上,位置用 `[f32; 3]`,朝向约定在模块文档里写明;
//! - 确定性:无随机、无浮点累加之外的隐藏状态,同输入逐位一致;
//! - 边界诚实:听者与声源重合、负距离、非有限输入、零长度朝向向量的
//!   行为必须显式定义并测试,不 panic。

/// 骨架身份标记(实现落地后删除)。
pub const SCAFFOLD: &str = "oriera_subsys::audio";

#[cfg(test)]
mod tests {
    #[test]
    fn scaffold_is_wired() {
        assert_eq!(super::SCAFFOLD, "oriera_subsys::audio");
    }
}
