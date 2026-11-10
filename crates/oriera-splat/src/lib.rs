//! L5 高斯泼溅(3D Gaussian Splatting)——经典 EWA 泼溅的 CPU 参考内核
//! (PLAN-splat v2 M0,**实验性**,契约 `oriera.render.splat.v1` 标 unstable)。
//!
//! 管线:[`decode_splat_ply`](L4 解码,KHR_gaussian_splatting 语义)→ [`project`]
//! (EWA 投影,剔除即 compaction)→ [`depth_order`](确定性前到背全序)→ [`rasterize`]
//! (透射率累积合成)。纯 Rust、零第三方依赖;输入相同则输出逐字节一致,且在深度两两
//! 不同时与输入 splat 的排列无关——[`SplatImage::fingerprint`] 可进回放审计。
//!
//! 诚实边界(详见 docs/STABILITY.md):
//! - 只有 EWA 投影(Jacobian 局部线性化):相机进入 splat 内部时会失真,射线类方法未实现;
//! - 颜色只用 SH DC 项,没有视角相关光照;
//! - CPU 光栅是参考/金样路径,复杂度 ∝ 覆盖像素总数,受 [`RasterBudget`] 约束,
//!   超预算返回 [`RasterError::OverBudget`]——绝不静默降级;
//! - GPU 路径已落地(PLAN-splat M1,feature `splat-wgpu` 默认关,见 [`gpu`]):
//!   确定性数学仍在 CPU,GPU 只做实例化四边形展开 + 后到前 over 混合,
//!   消费 #152 底座 [`oriera_render::camera::Camera`](经
//!   [`SplatCamera::from_view`] 换算),与 CPU 参考按评审门槛(99.9%≤2/255、
//!   RMS≤1/255)互证;
//! - 不读也不置 `NEURAL_RENDER_3DGS` 能力位:经典泼溅是确定性传统光栅化,属 ADR 0002
//!   降级语义里的 passthrough 侧,该位留给未来的神经精修 Pass。
//!
//! 出处:Kerbl et al. 2023(3DGS)、Zwicker et al. 2001(EWA Splatting);排序键打包借鉴
//! World Labs Spark/Radix(MIT)公开文档的工程模式。实现为原创,未拷贝第三方源代码。

pub mod camera;
#[cfg(feature = "splat-wgpu")]
pub mod gpu;
pub mod project;
pub mod raster;
pub mod scene;
pub mod sort;

pub use camera::SplatCamera;
pub use oriera_asset::ply::{decode_splat_ply, PlyError, SplatCloud, SH_C0};
pub use project::{project, ProjectedSplat};
pub use raster::{rasterize, RasterBudget, RasterError, RasterOptions, SplatImage};
pub use sort::{depth_order, sort_key};

#[cfg(feature = "splat-wgpu")]
pub use gpu::{GpuError, SplatRenderer};
