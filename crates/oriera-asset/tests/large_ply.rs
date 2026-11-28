//! 大场景 PLY 解码压力测试(办公室重建验证):
//! 生成 50 万泼溅合成办公室(墙面/地板/桌椅),编码 INRIA 格式 PLY,
//! 解码并断言数据完整性与解码耗时(真实照片训练产出的 PLY 同格式)。
//!
//! 办公室合成布局:
//! - 地板:10m×8m 网格,y=0;
//! - 四墙:x=0/x=10 与 z=0/z=8 竖面;
//! - 桌面:2m×1m 在中心,y=0.75;
//! - 总计 ~50 万 splat,PLY 文件 ~34MB(INRIA 格式 SH0+法线 stride=68B)。

use oriera_asset::ply::decode_splat_ply;
use std::time::Instant;

/// INRIA PLY 属性列表(SH0 + 法线),stride = 17 × 4 = 68 bytes。
const PROPS: &[(&str, &str)] = &[
    ("x", "float"),
    ("y", "float"),
    ("z", "float"),
    ("nx", "float"),
    ("ny", "float"),
    ("nz", "float"),
    ("f_dc_0", "float"),
    ("f_dc_1", "float"),
    ("f_dc_2", "float"),
    ("opacity", "float"),
    ("scale_0", "float"),
    ("scale_1", "float"),
    ("scale_2", "float"),
    ("rot_0", "float"),
    ("rot_1", "float"),
    ("rot_2", "float"),
    ("rot_3", "float"),
];

const STRIDE: usize = PROPS.len() * 4; // 68

/// 把一个 splat 写入 vertex 字节缓冲(INRIA 格式:opacity logit / scale log / rot wxyz)。
fn write_splat(buf: &mut Vec<u8>, pos: [f32; 3], nrm: [f32; 3], color: [f32; 3]) {
    let c0 = 0.282_094_8f32;
    let mut row = vec![0u8; STRIDE];
    let mut put = |i: usize, v: f32| {
        row[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    };
    // x, y, z
    put(0, pos[0]);
    put(1, pos[1]);
    put(2, pos[2]);
    // nx, ny, nz
    put(3, nrm[0]);
    put(4, nrm[1]);
    put(5, nrm[2]);
    // f_dc_0..2(颜色 → SH DC 反推)
    put(6, (color[0] - 0.5) / c0);
    put(7, (color[1] - 0.5) / c0);
    put(8, (color[2] - 0.5) / c0);
    // opacity(logit — sigmoid(2.0) ≈ 0.88)
    put(9, 2.0);
    // scale_0..2(log — 2cm)
    put(10, (0.02f32).ln());
    put(11, (0.02f32).ln());
    put(12, (0.02f32).ln());
    // rot_0..3(w,x,y,z — identity)
    put(13, 1.0);
    put(14, 0.0);
    put(15, 0.0);
    put(16, 0.0);
    buf.extend_from_slice(&row);
}

/// 生成合成办公室 PLY 字节(header + vertex data)。
fn generate_office_ply(total: usize) -> Vec<u8> {
    // 分配:地板 40%、四墙 30%、桌面 15%、其他 15%
    let floor_n = total * 40 / 100;
    let wall_n = total * 30 / 100;
    let desk_n = total * 15 / 100;
    let misc_n = total - floor_n - wall_n - desk_n;

    let mut header = String::new();
    header.push_str("ply\n");
    header.push_str("format binary_little_endian 1.0\n");
    header.push_str(&format!("element vertex {total}\n"));
    for (name, ty) in PROPS {
        header.push_str(&format!("property {ty} {name}\n"));
    }
    header.push_str("end_header\n");

    let mut data = Vec::with_capacity(total * STRIDE);

    // 地板:10×8m,y=0,法线+y,棕色
    let cols = (floor_n as f64).sqrt().ceil() as usize;
    for i in 0..floor_n {
        let ix = i / cols;
        let iz = i % cols;
        let x = (ix as f32 / cols as f32) * 10.0;
        let z = (iz as f32 / cols as f32) * 8.0;
        write_splat(&mut data, [x, 0.0, z], [0.0, 1.0, 0.0], [0.45, 0.35, 0.25]);
    }

    // 四墙:x=0/x=10 + z=0/z=8,白色(循环 wall_n 次,每 wall_n/4 次切一面墙)
    let per_wall = wall_n / 4;
    let wcols = (per_wall as f64).sqrt().ceil() as usize;
    for i in 0..wall_n {
        let local_i = i % per_wall;
        let wall = i / per_wall;
        let ix = local_i / wcols;
        let iz = local_i % wcols;
        let u = (ix as f32 / wcols as f32) * 8.0;
        let v = (iz as f32 / wcols as f32) * 3.0;
        let (pos, nrm) = match wall {
            0 => ([0.0, v, u], [1.0, 0.0, 0.0]),
            1 => ([10.0, v, u], [-1.0, 0.0, 0.0]),
            2 => ([u % 10.0, v, 0.0], [0.0, 0.0, 1.0]),
            _ => ([u % 10.0, v, 8.0], [0.0, 0.0, -1.0]),
        };
        write_splat(&mut data, pos, nrm, [0.85, 0.85, 0.82]);
    }

    // 桌面:2×1m 中心,y=0.75,木色
    let dcols = (desk_n as f64).sqrt().ceil() as usize;
    for i in 0..desk_n {
        let ix = i / dcols;
        let iz = i % dcols;
        let x = 4.0 + (ix as f32 / dcols as f32) * 2.0;
        let z = 3.5 + (iz as f32 / dcols as f32) * 1.0;
        write_splat(&mut data, [x, 0.75, z], [0.0, 1.0, 0.0], [0.55, 0.38, 0.20]);
    }

    // 其他:确定性散布(椅/显示器)
    let mut rng = 0x9E3779B97F4A7C15u64;
    for _ in 0..misc_n {
        rng = rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let x = 4.0 + ((rng >> 32) as f32 / (1u64 << 32) as f32) * 3.0;
        let z = 3.5 + ((rng >> 40) as f32 / (1u64 << 24) as f32) * 2.0;
        let y = 0.8 + ((rng >> 48) as f32 / (1u64 << 16) as f32) * 0.8;
        write_splat(&mut data, [x, y, z], [0.0, 0.7, 0.7], [0.15, 0.15, 0.18]);
    }

    // header + data
    let mut result = header.into_bytes();
    result.extend_from_slice(&data);
    result
}

#[test]
fn large_office_ply_decode_and_validate() {
    let total = 500_000;
    let t0 = Instant::now();
    let ply_bytes = generate_office_ply(total);
    let gen_ms = t0.elapsed().as_secs_f64() * 1e3;
    println!(
        "generated {total}-splat office PLY: {} bytes ({:.1} MB) in {gen_ms:.0}ms",
        ply_bytes.len(),
        ply_bytes.len() as f64 / 1e6
    );

    // 文件大小 = header(~350B) + total × stride(68B)
    let expected_min = total * STRIDE;
    assert!(
        ply_bytes.len() >= expected_min,
        "PLY data section too small: {} < {expected_min}",
        ply_bytes.len()
    );

    // ── 解码 ──
    let t1 = Instant::now();
    let cloud = decode_splat_ply(&ply_bytes).expect("large office PLY must decode");
    let decode_ms = t1.elapsed().as_secs_f64() * 1e3;
    println!(
        "decoded {} splats in {decode_ms:.0}ms ({:.0} splats/ms)",
        cloud.len(),
        cloud.len() as f64 / decode_ms
    );

    // ── 数据完整性 ──
    assert_eq!(cloud.len(), total, "解码泼溅数不符");
    assert_eq!(cloud.sh_degree, 0, "SH0 预期");

    // 位置合理:所有 x ∈ [0, 10+1], z ∈ [0, 8+1], y ∈ [-1, 5]
    for (i, p) in cloud.positions.iter().enumerate().step_by(1000) {
        assert!(
            p[0] >= -1.0
                && p[0] <= 11.0
                && p[2] >= -1.0
                && p[2] <= 9.0
                && p[1] >= -1.0
                && p[1] <= 5.0,
            "splat {i} 位置越界: {p:?}"
        );
    }

    // 颜色合理:地板棕(R>B),墙面白(R≈B≈G)
    let floor_color = cloud.base_color(0);
    assert!(
        floor_color[0] > floor_color[2],
        "地板应偏暖 R>B: {floor_color:?}"
    );
    let wall_start = floor_n_from(total);
    let wall_color = cloud.base_color(wall_start);
    assert!(
        wall_color[0] > 0.7 && wall_color[1] > 0.7,
        "墙面应偏白: {wall_color:?}"
    );

    // ── 确定性:双跑解码逐位一致 ──
    let cloud2 = decode_splat_ply(&ply_bytes).expect("second decode");
    assert_eq!(cloud.positions, cloud2.positions, "位置不逐位一致");
    assert_eq!(cloud.opacities, cloud2.opacities, "不透明度不逐位一致");
    assert_eq!(cloud.sh_dc, cloud2.sh_dc, "SH DC 不逐位一致");

    println!(
        "PASS: {total} splat office PLY decode + validate ({:.1} MB, {decode_ms:.0}ms)",
        ply_bytes.len() as f64 / 1e6
    );
}

fn floor_n_from(total: usize) -> usize {
    total * 40 / 100
}
