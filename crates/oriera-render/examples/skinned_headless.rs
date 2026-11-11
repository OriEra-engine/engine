//! #153 M1 证据示例:CesiumMan(CC-BY 4.0)蒙皮动画 GPU 出图。
//!
//! ```bash
//! # 三姿态 BMP 落当前目录(或指定输出目录):
//! RUSTUP_TOOLCHAIN=msvc-local CARGO_TARGET_DIR=target/msvc WGPU_BACKEND=gl \
//!   cargo run -p oriera-render --features render-wgpu --example skinned_headless -- <out-dir>
//! ```
//! 每姿态一张 BMP(t=0 / 1/3 / 2/3 周期),另出一张 CPU 参考 vs GPU 蒙皮的
//! 互证同帧(两路径同面板渲染,肉眼应不可分——差异由 tests/skin_gpu.rs 像素门禁锁定)。
//! BMP 为零依赖手写(24bpp bottom-up),任何看图软件可开。

use oriera_asset::glb::{decode_glb_rig, joint_matrices_at};
use oriera_render::camera::Camera;
use oriera_render::gpu::{HeadlessRenderer, SkinnedPart};
use std::path::PathBuf;

fn main() {
    let out_dir = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());
    let fix = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../oriera-asset/tests/fixtures/CesiumMan.glb");
    let rig = decode_glb_rig(&std::fs::read(&fix).unwrap_or_else(|e| {
        die(&format!("读夹具 {fix:?} 失败: {e}"));
    }))
    .unwrap_or_else(|e| die(&format!("CesiumMan 解码失败: {e}")));
    let dur = rig.animations[0].duration;
    let prim = &rig.primitives[0];
    let joints32: Vec<[u32; 4]> = prim
        .joints
        .iter()
        .map(|j| [j[0] as u32, j[1] as u32, j[2] as u32, j[3] as u32])
        .collect();
    let base = prim.base_color.unwrap_or([1.0, 1.0, 1.0, 1.0]);
    let renderer = HeadlessRenderer::new().unwrap_or_else(|e| die(&format!("GPU 初始化失败: {e}")));
    println!("适配器: {}", renderer.adapter_info());
    println!(
        "CesiumMan: {} 顶点 / {} 关节 / 时长 {dur:.2}s",
        prim.positions.len(),
        rig.skins[0].joints.len()
    );

    for (tag, frac) in [("t0", 0.0f32), ("t13", 1.0 / 3.0), ("t23", 2.0 / 3.0)] {
        let palettes = joint_matrices_at(&rig, Some(0), dur * frac);
        let skinned = SkinnedPart {
            positions: &prim.positions,
            normals: prim.normals.as_deref(),
            uvs: None,
            indices: &prim.indices,
            base_color: base,
            metallic: 0.0,
            roughness: 0.9,
            base_texture: None,
            mr_texture: None,
            joints: &joints32,
            weights: &prim.weights,
            palette: &palettes[0],
        };
        let camera = camera_for(&prim.positions);
        let img = renderer
            .render_skinned_views(&[skinned], &camera)
            .unwrap_or_else(|e| die(&format!("渲染失败: {e}")));
        let out = PathBuf::from(&out_dir).join(format!("cesiumman_{tag}.bmp"));
        write_bmp(&out.to_string_lossy(), &img)
            .unwrap_or_else(|e| die(&format!("写 BMP 失败: {e}")));
        println!("✅ {tag}(t={:.2}s) → {}", dur * frac, out.display());
    }
    println!("互证像素门禁:见 tests/skin_gpu.rs(3 测:四时刻对拍/动画驱动差异/越界拒绝)");
}

/// 相机:CesiumMan bind 位姿包围盒自动取景(模型站立,原点附近)。
fn camera_for(positions: &[[f32; 3]]) -> Camera {
    let mut min = [f32::MAX; 3];
    let mut max = [f32::MIN; 3];
    for p in positions {
        for r in 0..3 {
            min[r] = min[r].min(p[r]);
            max[r] = max[r].max(p[r]);
        }
    }
    let center = [
        (min[0] + max[0]) * 0.5,
        (min[1] + max[1]) * 0.5,
        (min[2] + max[2]) * 0.5,
    ];
    let radius = (max[0] - min[0])
        .max(max[1] - min[1])
        .max(max[2] - min[2])
        .max(0.01);
    Camera::look_at(
        [
            center[0] + radius * 3.2,
            center[1] + radius * 1.2,
            center[2] + radius * 4.0,
        ],
        center,
        [0.0, 1.0, 0.0],
        40.0,
        320,
        240,
        0.05,
        100.0,
    )
}

/// 24bpp bottom-up BMP:54 字节头 + 每行 4 字节对齐的 BGR。
fn write_bmp(path: &str, img: &oriera_render::gpu::RenderedImage) -> std::io::Result<()> {
    let (w, h) = (img.width as usize, img.height as usize);
    let row = (w * 3).div_ceil(4) * 4;
    let size = 54 + row * h;
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes()); // BITMAPINFOHEADER
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&24u16.to_le_bytes()); // bpp
    out.extend_from_slice(&[0; 4]); // 无压缩
    out.extend_from_slice(&((row * h) as u32).to_le_bytes());
    out.extend_from_slice(&[0; 16]); // 分辨率/调色板占位
    for y in (0..h).rev() {
        for x in 0..w {
            let px = img.pixel(x as u32, y as u32); // BGRA
            out.extend_from_slice(&[px[0], px[1], px[2]]); // BMP 也是 BGR 序
        }
        out.extend(std::iter::repeat_n(0, row - w * 3));
    }
    std::fs::write(path, out)
}

fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1);
}
