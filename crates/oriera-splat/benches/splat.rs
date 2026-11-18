//! oriera-splat 基准:为 docs/STABILITY.md 的 CPU 上限提供实测数字(PLAN-splat v2 §2.4)。
//! 运行:`cargo bench -p oriera-splat`。

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use oriera_splat::{
    decode_splat_ply, depth_order, project, rasterize, scene, ProjectedSplat, RasterOptions,
    SplatCloud,
};
use std::hint::black_box;

/// 合成投影结果:深度均匀散布,只为测排序本身。
fn synthetic(n: u32) -> Vec<ProjectedSplat> {
    let mut state = 0x0bad_5eed_u64;
    (0..n)
        .map(|index| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ProjectedSplat {
                index,
                depth: 0.2 + (state >> 40) as f32 / 1.0e5,
                mean: [0.0; 2],
                conic: [1.0, 0.0, 1.0],
                extent: [3.0; 2],
                color: [1.0; 3],
                opacity: 0.5,
            }
        })
        .collect()
}

fn sort(c: &mut Criterion) {
    let mut group = c.benchmark_group("depth_order");
    for n in [10_000u32, 100_000, 1_000_000] {
        let projected = synthetic(n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &projected, |b, p| {
            b.iter(|| depth_order(black_box(p)))
        });
    }
    group.finish();
}

fn projection(c: &mut Criterion) {
    let cloud = scene::petal_ring(100_000, 7);
    let camera = scene::petal_camera(1280, 720);
    c.bench_function("project/100000", |b| {
        b.iter(|| project(black_box(&cloud), &camera))
    });
}

fn raster(c: &mut Criterion) {
    let cloud = scene::petal_ring(5_000, 7);
    let opts = RasterOptions {
        background: scene::BRAND_BACKGROUND,
        ..Default::default()
    };
    for (w, h) in [(640, 360), (1280, 720)] {
        let camera = scene::petal_camera(w, h);
        c.bench_function(&format!("rasterize/5000@{w}x{h}"), |b| {
            b.iter(|| rasterize(black_box(&cloud), &camera, &opts).unwrap())
        });
    }
}

fn ply(c: &mut Criterion) {
    let bytes = encode(&scene::petal_ring(100_000, 7));
    c.bench_function("decode_splat_ply/100000", |b| {
        b.iter(|| decode_splat_ply(black_box(&bytes)).unwrap())
    });
}

/// 最小 INRIA 布局写出器(只为基准造输入):激活值反变换回文件域。
fn encode(cloud: &SplatCloud) -> Vec<u8> {
    let names = [
        "x", "y", "z", "f_dc_0", "f_dc_1", "f_dc_2", "opacity", "scale_0", "scale_1", "scale_2",
        "rot_0", "rot_1", "rot_2", "rot_3",
    ];
    let mut head = format!(
        "ply\nformat binary_little_endian 1.0\nelement vertex {}\n",
        cloud.len()
    );
    for name in names {
        head.push_str(&format!("property float {name}\n"));
    }
    head.push_str("end_header\n");
    let mut out = head.into_bytes();
    let rows = cloud
        .positions
        .iter()
        .zip(&cloud.sh_dc)
        .zip(&cloud.opacities)
        .zip(&cloud.scales)
        .zip(&cloud.rotations);
    for ((((p, dc), &o), s), &[qx, qy, qz, qw]) in rows {
        let o = o.clamp(1e-6, 1.0 - 1e-6);
        let row = [
            p[0],
            p[1],
            p[2],
            dc[0],
            dc[1],
            dc[2],
            (o / (1.0 - o)).ln(),
            s[0].ln(),
            s[1].ln(),
            s[2].ln(),
            qw,
            qx,
            qy,
            qz,
        ];
        for v in row {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

criterion_group!(benches, sort, projection, raster, ply);
criterion_main!(benches);
