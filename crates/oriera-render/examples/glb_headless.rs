//! GLB → 无头渲染 → BMP:wgpu 最小闭环的自证示例。
//!
//! ```bash
//! # 内置三角:
//! cargo run -p oriera-render --example glb_headless --features render-wgpu
//! # 渲染任意 GLB(Tripo/L8 生成物或普通模型):
//! cargo run -p oriera-render --example glb_headless --features render-wgpu -- model.glb out.bmp
//! ```
//! BMP 为零依赖手写(24bpp bottom-up),任何看图软件可开。

// 需要 --features render-wgpu 才会参与编译(Cargo.toml required-features)。
use oriera_render::gpu::{GpuError, HeadlessRenderer};

const W: u32 = 800;
const H: u32 = 600;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (positions, indices, label) = match args.get(1).map(String::as_str) {
        Some(path) => {
            let bytes =
                std::fs::read(path).unwrap_or_else(|e| die(&format!("读 {path} 失败: {e}")));
            let mesh = oriera_asset::glb::decode_glb_mesh(&bytes)
                .unwrap_or_else(|e| die(&format!("GLB 解码失败: {e}")));
            println!(
                "GLB: {} 顶点 / {} 索引,包围盒 {:?}..{:?}",
                mesh.positions.len(),
                mesh.indices.len(),
                mesh.min,
                mesh.max
            );
            (mesh.positions, mesh.indices, path.to_string())
        }
        None => {
            println!("未指定 GLB,渲染内置三角(传入路径可渲染任意 GLB)");
            (
                vec![[-1.0, -0.5, 0.0], [1.0, -0.5, 0.0], [0.0, 0.8, 0.0]],
                vec![0, 1, 2],
                "builtin-triangle".to_string(),
            )
        }
    };
    let out = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "oriera-headless.bmp".to_string());

    let renderer = HeadlessRenderer::new().unwrap_or_else(|e| die(&format!("GPU 初始化失败: {e}")));
    println!("适配器: {}", renderer.adapter_info());

    let img = renderer
        .render_mesh(&positions, &indices, W, H)
        .unwrap_or_else(|e: GpuError| die(&format!("渲染失败: {e}")));

    write_bmp(&out, &img).unwrap_or_else(|e| die(&format!("写 BMP 失败: {e}")));
    println!(
        "✅ {label} → {out} ({}×{},BGRA {}KB)",
        img.width,
        img.height,
        img.bgra.len() / 1024
    );
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
