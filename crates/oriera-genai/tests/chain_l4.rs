//! L8→L4→GLB 解码全链集成测试(测评 P0「资产生成链断」的闭环证明)。
//!
//! MockProvider(离线、确定性)走完整的任务生命周期,产物经
//! [`validate_asset`] 守门后按逻辑名落进 L4 [`AssetStore`],再由
//! L4 消费方(L5 的 GLB 解码器)接手,得到可直接渲染的 mesh。
//! 全程无网络、无 key、无 GPU —— CI 可跑;最后一步 GPU 光栅化见
//! `oriera-render` 的 `glb_headless` 示例。

use oriera_asset::glb::decode_glb_mesh;
use oriera_asset::AssetStore;
use oriera_genai::{
    validate_asset, AssetFormat, GenerationProvider, GenerationRequest, MockProvider, TaskStatus,
    ValidationLimits,
};

fn generate_and_land(name: &str, store: &mut AssetStore) {
    let provider = MockProvider::new();
    let req = GenerationRequest::text_to_3d(name, "一只圆脸机器人管家");
    let task = provider.request_generation(&req).expect("mock serves 3d");

    // 生命周期:至少一次非终态 —— 生成中的任务不许被当成资产。
    let first = provider.poll_task(&task).unwrap();
    assert!(!first.is_terminal(), "paced mock starts non-terminal");
    let done = loop {
        match provider.poll_task(&task).unwrap() {
            TaskStatus::Succeeded(a) => break a,
            TaskStatus::Failed(e) => panic!("mock generation failed: {e}"),
            _ => {}
        }
    };
    assert_eq!(done.format, AssetFormat::Glb);

    // 引擎边界唯一守门:validate 通过才允许落 L4。
    validate_asset(&done, &ValidationLimits::default()).expect("placeholder glb validates");
    store.insert_named(name, done.payload);
}

#[test]
fn generated_glb_lands_in_l4_and_decodes_to_a_mesh() {
    let mut store = AssetStore::new();
    generate_and_land("npc_face_v1", &mut store);

    let landed: &Vec<u8> = store
        .get_named("npc_face_v1")
        .expect("named asset resolvable after landing");
    let mesh = decode_glb_mesh(landed).expect("landed GLB decodes to a renderable mesh");
    assert_eq!(mesh.positions.len(), 3, "one visible triangle placeholder");
    assert_eq!(mesh.indices.len(), 3);
    assert_eq!(mesh.min, [-0.8, -0.6, 0.0]);
    assert_eq!(mesh.max, [0.8, 0.6, 0.0]);
}

#[test]
fn regenerating_swaps_the_payload_behind_the_same_name() {
    // 生成 v2 覆盖同名 v1:调用方继续用同一逻辑名解析,原位换芯、
    // 不重建实体 —— 这是 L8 生成管线往世界里灌资产的唯一通道。
    let mut store = AssetStore::new();
    generate_and_land("npc_face", &mut store);
    let v1: &Vec<u8> = store.get_named("npc_face").unwrap();
    let digest_v1 = v1.len();

    generate_and_land("npc_face", &mut store);
    let v2: &Vec<u8> = store.get_named("npc_face").unwrap();
    // 同一 mock 产物字节相同 —— 断言解析路径稳定,热换语义由
    // insert_named 的句柄复用保证(单测覆盖于 oriera-asset)。
    assert_eq!(v2.len(), digest_v1);
    assert!(decode_glb_mesh(v2).is_ok());
}

#[test]
fn corrupt_payload_never_reaches_l4_named_storage() {
    // 守门在先:validate 拒绝的产物根本不会落地(截断的 GLB)。
    let provider = MockProvider::new();
    let req = GenerationRequest::text_to_3d("broken", "x");
    let task = provider.request_generation(&req).unwrap();
    let done = loop {
        if let TaskStatus::Succeeded(a) = provider.poll_task(&task).unwrap() {
            break a;
        }
    };
    let mut truncated = done.payload.clone();
    truncated.truncate(truncated.len() - 8);
    let bad = oriera_genai::GeneratedAsset {
        format: done.format,
        payload: truncated,
        provider_id: done.provider_id.clone(),
        model: done.model.clone(),
        usage: None,
    };
    assert!(validate_asset(&bad, &ValidationLimits::default()).is_err());
}
