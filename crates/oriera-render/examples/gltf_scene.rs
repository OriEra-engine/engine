//! 原生 glTF 全场景出图(#152 第二刀证据链):GLB → L4 场景解码 → L5 纹理/相机渲染 → BMP。
//!
//! ```text
//! cargo run -p oriera-render --release --features render-wgpu \
//!   --example gltf_scene -- --glb PATH --out out.bmp [--size WxH] \
//!                          [--eye x,y,z --target x,y,z --fov DEG]
//! ```
//!
//! 纹理解码需 `oriera-asset` 的 `texture` feature(见 dev-dependencies)。
//! 相机缺省自动取景:逐轴中位数为中心、80 分位距离定半径(与 splat_cpu 同式)。

use oriera_asset::glb::{decode_glb_scene, decode_png_texture};
use oriera_render::gpu::GpuTexture;
use oriera_render::gpu::{HeadlessRenderer, ScenePart};
use std::time::Instant;

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let glb_path = get("--glb").ok_or("--glb PATH 必填")?;
    let out = get("--out").unwrap_or_else(|| "gltf_scene.bmp".into());
    let (width, height) = match get("--size") {
        Some(s) => {
            let (w, h) = s.split_once('x').ok_or("--size expects WxH")?;
            (
                w.parse::<u32>().map_err(|e| format!("{e}"))?,
                h.parse::<u32>().map_err(|e| format!("{e}"))?,
            )
        }
        None => (960, 540),
    };

    let bytes = std::fs::read(&glb_path).map_err(|e| format!("{glb_path}: {e}"))?;
    let t0 = Instant::now();
    let scene = decode_glb_scene(&bytes).map_err(|e| format!("{glb_path}: {e}"))?;
    println!(
        "{} | {} primitives | decode {:.1} ms",
        glb_path,
        scene.primitives.len(),
        t0.elapsed().as_secs_f64() * 1e3
    );

    let renderer = HeadlessRenderer::new().map_err(|e| e.to_string())?;
    println!("adapter: {}", renderer.adapter_info());

    // 上传纹理(有 baseColorTexture 的 primitive 才需要)
    let mut textures: Vec<Option<GpuTexture>> = Vec::with_capacity(scene.primitives.len());
    let mut textured = 0usize;
    for prim in &scene.primitives {
        match &prim.base_color_texture {
            Some(tex_bytes) => {
                let t = decode_png_texture(tex_bytes).map_err(|e| format!("纹理解码: {e}"))?;
                textured += 1;
                textures.push(Some(
                    renderer
                        .upload_texture(&t.rgba, t.width, t.height)
                        .map_err(|e| e.to_string())?,
                ));
            }
            None => textures.push(None),
        }
    }
    println!(
        "{textured}/{}/{} primitives 带纹理",
        textured,
        scene.primitives.len()
    );

    // 场景 parts:基色缺省按规范取 [1,1,1,1]
    let parts: Vec<ScenePart<'_>> = scene
        .primitives
        .iter()
        .zip(&textures)
        .map(|(p, tex)| ScenePart {
            positions: &p.positions,
            normals: p.normals.as_deref(),
            uvs: p.uvs.as_deref(),
            indices: &p.indices,
            base_color: p.base_color.unwrap_or([1.0, 1.0, 1.0, 1.0]),
            texture: tex.as_ref(),
        })
        .collect();

    // 自动取景:全场景实算包围盒
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for p in &scene.primitives {
        for i in 0..3 {
            min[i] = min[i].min(p.min[i]);
            max[i] = max[i].max(p.max[i]);
        }
    }
    let center = [
        (min[0] + max[0]) * 0.5,
        (min[1] + max[1]) * 0.5,
        (min[2] + max[2]) * 0.5,
    ];
    let radius =
        ((max[0] - min[0]).powi(2) + (max[1] - min[1]).powi(2) + (max[2] - min[2]).powi(2)).sqrt()
            * 0.5;
    let parse_vec = |s: &str| -> Result<[f32; 3], String> {
        let v: Vec<f32> = s
            .split(',')
            .map(|p| p.trim().parse::<f32>().map_err(|e| format!("`{s}`: {e}")))
            .collect::<Result<_, _>>()?;
        v.try_into()
            .map_err(|_| format!("`{s}`: 期望三个逗号分隔数"))
    };
    let target = match get("--target") {
        Some(s) => parse_vec(&s)?,
        None => center,
    };
    let eye = match get("--eye") {
        Some(s) => parse_vec(&s)?,
        None => {
            let d = radius.max(1e-3) * 2.6;
            [target[0], target[1] + d * 0.45, target[2] - d * 0.85]
        }
    };
    let fov = get("--fov").map_or(45.0, |s| s.parse::<f32>().unwrap_or(45.0));
    let camera = oriera_render::camera::Camera::look_at(
        eye,
        target,
        [0.0, 1.0, 0.0],
        fov,
        width,
        height,
        0.05,
        radius.max(1.0) * 20.0,
    );

    let t1 = Instant::now();
    let image = renderer
        .render_scene_views(&parts, &camera)
        .map_err(|e| e.to_string())?;
    println!(
        "{width}x{height} | render {:.1} ms | fingerprint {:016x}",
        t1.elapsed().as_secs_f64() * 1e3,
        0 // RenderedImage 无指纹(泼溅图专属)
    );
    write_bmp(&out, &image).map_err(|e| format!("{out}: {e}"))?;
    println!("wrote {out}");
    Ok(())
}

/// 24 位无压缩 BMP(自下而上存行、BGR、行宽 4 字节对齐)。
fn write_bmp(path: &str, img: &oriera_render::gpu::RenderedImage) -> std::io::Result<()> {
    let (w, h) = (img.width as usize, img.height as usize);
    let row = (w * 3 + 3) & !3;
    let size = 54 + row * h;
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    for y in (0..img.height).rev() {
        let start = out.len();
        for x in 0..img.width {
            let [b, g, r, _] = img.pixel(x, y);
            out.extend_from_slice(&[b, g, r]);
        }
        out.resize(start + row, 0);
    }
    std::fs::write(path, out)
}
