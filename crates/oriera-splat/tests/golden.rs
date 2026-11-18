//! 金样:固定程序化场景的 CPU 参考出图,与入库参考逐像素比对。每通道容差 1/255,
//! 吸收跨平台 libm(`exp`)的末位差;同平台的逐字节确定性由单元测试另行锁定。
//!
//! 重锚(仅在有意改变渲染语义时):
//! `ORIERA_SPLAT_BLESS=1 cargo test -p oriera-splat --test golden`

use oriera_splat::{rasterize, scene, RasterOptions};
use std::path::PathBuf;

const W: u32 = 64;
const H: u32 = 48;

#[test]
fn petal_ring_matches_golden_within_one_level() {
    let cloud = scene::petal_ring(600, 2026);
    let camera = scene::petal_camera(W, H);
    let opts = RasterOptions {
        background: scene::BRAND_BACKGROUND,
        ..Default::default()
    };
    let image = rasterize(&cloud, &camera, &opts).expect("scene fits the default budget");

    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/petal_ring_64x48.rgba");
    if std::env::var_os("ORIERA_SPLAT_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &image.rgba).unwrap();
    }
    let golden = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "missing golden {} ({e}); bless with ORIERA_SPLAT_BLESS=1",
            path.display()
        )
    });
    assert_eq!(golden.len(), image.rgba.len(), "golden size mismatch");
    let worst = golden
        .iter()
        .zip(&image.rgba)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
    assert!(worst <= 1, "max channel delta {worst}/255 exceeds 1/255");

    // 金样必须真有内容:至少 1/10 像素被覆盖。
    let covered = image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] > 0)
        .count();
    assert!(
        covered * 10 >= (W * H) as usize,
        "golden scene too sparse: {covered} px covered"
    );
}
