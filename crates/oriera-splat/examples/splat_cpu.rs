//! CPU 参考光栅出图(PLAN-splat v2 §2.4 证据链):PLY 或程序化场景 → BMP。
//!
//! ```text
//! cargo run -p oriera-splat --release --example splat_cpu -- [选项]
//!   --ply PATH            读 3DGS PLY(缺省 = 程序化花瓣环 4000 splat)
//!   --out PATH            输出 BMP(缺省 splat_cpu.bmp)
//!   --size WxH            分辨率(缺省 960x540)
//!   --max-splats N        显式放宽 CPU 预算的 splat 上限(缺省 5000;超限报错而非降级)
//!   --max-pixels N        显式放宽像素上限(缺省 1280×720)
//!   --eye x,y,z --target x,y,z --up x,y,z --fov DEG   相机(PLY 缺省自动取景)
//! ```

use oriera_splat::{
    decode_splat_ply, project, rasterize, scene, RasterBudget, RasterOptions, SplatCamera,
    SplatCloud, SplatImage,
};
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
    let (width, height) = match get("--size") {
        Some(s) => parse_size(&s)?,
        None => (960, 540),
    };
    let out = get("--out").unwrap_or_else(|| "splat_cpu.bmp".into());

    let started = Instant::now();
    let ply = get("--ply");
    let cloud = match &ply {
        Some(path) => {
            let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
            decode_splat_ply(&bytes).map_err(|e| format!("{path}: {e}"))?
        }
        None => scene::petal_ring(4000, 2026),
    };
    println!(
        "splats {} | SH degree {} | decode {:.1} ms",
        cloud.len(),
        cloud.sh_degree,
        started.elapsed().as_secs_f64() * 1e3
    );
    for line in &cloud.comments {
        println!("  {line}");
    }

    let camera = camera_for(&cloud, &get, width, height, ply.is_none())?;
    let mut budget = RasterBudget::default();
    if let Some(n) = get("--max-splats") {
        budget.max_splats = n.parse().map_err(|e| format!("--max-splats: {e}"))?;
    }
    if let Some(n) = get("--max-pixels") {
        budget.max_pixels = n.parse().map_err(|e| format!("--max-pixels: {e}"))?;
    }
    let opts = RasterOptions {
        background: scene::BRAND_BACKGROUND,
        budget,
    };

    let visible = project(&cloud, &camera).len();
    let started = Instant::now();
    let image = rasterize(&cloud, &camera, &opts).map_err(|e| e.to_string())?;
    let render_ms = started.elapsed().as_secs_f64() * 1e3;
    let covered = image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] > 0)
        .count();
    println!(
        "{width}x{height} | visible {visible} | covered {covered} px | render {render_ms:.1} ms | fingerprint {:016x}",
        image.fingerprint()
    );
    write_bmp(&out, &image).map_err(|e| format!("{out}: {e}"))?;
    println!("wrote {out}");
    Ok(())
}

fn camera_for(
    cloud: &SplatCloud,
    get: &dyn Fn(&str) -> Option<String>,
    width: u32,
    height: u32,
    procedural: bool,
) -> Result<SplatCamera, String> {
    let fov = match get("--fov") {
        Some(s) => s.parse::<f32>().map_err(|e| format!("--fov: {e}"))?,
        None => 45.0,
    };
    let eye = get("--eye").map(|s| parse_vec(&s)).transpose()?;
    if procedural && eye.is_none() {
        return Ok(scene::petal_camera(width, height));
    }
    // 自动取景:逐轴中位数作中心(抗漂浮噪点),80 分位距离作半径。
    let (center, radius) = frame(cloud);
    println!(
        "frame center ({:.3}, {:.3}, {:.3}) radius {radius:.3}",
        center[0], center[1], center[2]
    );
    let target = get("--target")
        .map(|s| parse_vec(&s))
        .transpose()?
        .unwrap_or(center);
    let up = get("--up")
        .map(|s| parse_vec(&s))
        .transpose()?
        .unwrap_or([0.0, 1.0, 0.0]);
    let eye = eye.unwrap_or([
        center[0],
        center[1] + 0.4 * radius,
        center[2] - 2.4 * radius,
    ]);
    Ok(SplatCamera::look_at(eye, target, up, fov, width, height))
}

fn frame(cloud: &SplatCloud) -> ([f32; 3], f32) {
    if cloud.is_empty() {
        return ([0.0; 3], 1.0);
    }
    let median = |axis: usize| {
        let mut v: Vec<f32> = cloud.positions.iter().map(|p| p[axis]).collect();
        v.sort_by(f32::total_cmp);
        v[v.len() / 2]
    };
    let center = [median(0), median(1), median(2)];
    let mut dist: Vec<f32> = cloud
        .positions
        .iter()
        .map(|p| {
            let d = [p[0] - center[0], p[1] - center[1], p[2] - center[2]];
            (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
        })
        .collect();
    dist.sort_by(f32::total_cmp);
    let radius = dist[dist.len() * 4 / 5].max(1e-3);
    (center, radius)
}

fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s.split_once('x').ok_or("--size expects WxH")?;
    let w = w.parse().map_err(|e| format!("--size width: {e}"))?;
    let h = h.parse().map_err(|e| format!("--size height: {e}"))?;
    Ok((w, h))
}

fn parse_vec(s: &str) -> Result<[f32; 3], String> {
    let v: Vec<f32> = s
        .split(',')
        .map(|p| p.trim().parse::<f32>().map_err(|e| format!("`{s}`: {e}")))
        .collect::<Result<_, _>>()?;
    v.try_into()
        .map_err(|_| format!("`{s}`: expected three comma-separated numbers"))
}

/// 24 位无压缩 BMP(自下而上存行、BGR、行宽 4 字节对齐)。
fn write_bmp(path: &str, img: &SplatImage) -> std::io::Result<()> {
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
            let [r, g, b, _] = img.pixel(x, y);
            out.extend_from_slice(&[b, g, r]);
        }
        out.resize(start + row, 0);
    }
    std::fs::write(path, out)
}
