#![cfg(feature = "render-wgpu")]
//! 白天化(scene batch)GPU 证据链:`SceneEnv` 参数化清屏/光向。
//!
//! 三条断言:
//! 1. env.clear 改变背景像素(参数真生效,非装饰);
//! 2. 同 env 双渲染逐像素一致(确定性红线延续);
//! 3. `render_scene_views`(DEFAULT 委托)与直调 `render_scene_views_env`
//!    (DEFAULT)逐位一致——历史调用的逐位兼容性不破。
//!
//! 无适配器环境(ubuntu CI runner)诚实跳过;有卡机器(含本机核显 DX12)
//! 真跑。

use oriera_render::camera::Camera;
use oriera_render::gpu::{GpuError, HeadlessRenderer, RenderedImage, SceneEnv, ScenePart};

fn renderer_or_skip() -> Option<HeadlessRenderer> {
    match HeadlessRenderer::new() {
        Ok(r) => Some(r),
        Err(GpuError::Device(msg)) if msg.contains("no usable adapter") => {
            eprintln!("skip: no GPU adapter on this runner");
            None
        }
        Err(e) => panic!("gpu init: {e}"),
    }
}

fn pixels_of(img: &RenderedImage) -> Vec<u8> {
    let mut v = Vec::with_capacity((img.width * img.height * 4) as usize);
    for y in 0..img.height {
        for x in 0..img.width {
            v.extend_from_slice(&img.pixel(x, y));
        }
    }
    v
}

/// 最小场景:一个三角形(局部=世界空间,面向相机)。
struct Tri {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    indices: Vec<u32>,
}

fn tri_scene() -> (Tri, Camera, Vec<ScenePart<'static>>) {
    let tri = Tri {
        positions: vec![[-0.25, -0.25, 0.5], [0.25, -0.25, 0.5], [0.0, 0.28, 0.5]],
        normals: vec![[0.0, 0.0, 1.0]; 3],
        indices: vec![0, 1, 2],
    };
    let camera = Camera::look_at(
        [0.0, 0.0, -1.5],
        [0.0, 0.0, 0.5],
        [0.0, 1.0, 0.0],
        50.0,
        320,
        240,
        0.1,
        10.0,
    );
    // 静态化以喂 ScenePart 的借用(测试内一次性构建,无运行时变更)。
    let positions: &'static Vec<[f32; 3]> = Box::leak(Box::new(tri.positions.clone()));
    let normals: &'static Vec<[f32; 3]> = Box::leak(Box::new(tri.normals.clone()));
    let indices: &'static Vec<u32> = Box::leak(Box::new(tri.indices.clone()));
    let parts = vec![ScenePart {
        positions,
        normals: Some(normals),
        uvs: None,
        indices,
        base_color: [0.8, 0.3, 0.2, 1.0],
        metallic: 0.0,
        roughness: 0.8,
        base_texture: None,
        mr_texture: None,
    }];
    (tri, camera, parts)
}

#[test]
fn env_parameterizes_background_and_default_delegates_bit_identically() {
    let Some(renderer) = renderer_or_skip() else {
        return;
    };
    let (_tri, camera, parts) = tri_scene();

    let legacy = renderer
        .render_scene_views(&parts, &camera)
        .expect("default legacy");
    let via_default = renderer
        .render_scene_views_env(&parts, &camera, &SceneEnv::DEFAULT)
        .expect("default delegated");
    let red = SceneEnv {
        clear: [1.0, 0.0, 0.0, 1.0],
        light_dir: [-0.45, 0.55, -0.70],
        light_intensity: 1.0,
    };
    let red_a = renderer
        .render_scene_views_env(&parts, &camera, &red)
        .expect("red clear");
    let red_b = renderer
        .render_scene_views_env(&parts, &camera, &red)
        .expect("red again");

    // (1) env.clear 真生效:背景角像素(BGRA)红=蓝通道位形明确区分。
    assert_ne!(
        legacy.pixel(2, 2),
        red_a.pixel(2, 2),
        "env.clear 未生效——背景像素与默认一致"
    );
    // 纯红清屏 → BGRA [0,0,255,255](Bgra8Unorm 直写,无伽马)。
    assert_eq!(red_a.pixel(2, 2), [0, 0, 255, 255]);

    // (2) 同 env 双渲染逐位一致。
    assert_eq!(pixels_of(&red_a), pixels_of(&red_b), "env 渲染双跑不一致");

    // (3) DEFAULT 委托 = 直调 DEFAULT,逐位等价——历史调用兼容性不破。
    assert_eq!(
        pixels_of(&legacy),
        pixels_of(&via_default),
        "DEFAULT 委托不等价——逐位兼容破线"
    );
}
