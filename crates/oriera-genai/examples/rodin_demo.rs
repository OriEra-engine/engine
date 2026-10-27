//! Hyper3D Rodin 接入效果演示：真实适配器代码全链（云端响应为脚本注入）。
//! 运行：cargo run -p oriera-genai --example rodin_demo --features providers

use oriera_genai::transport::{HttpRequest, HttpResponse, SharedTransport, Transport};
use oriera_genai::{GenerationRequest, ProviderConfig, ProviderKind, TaskStatus};
use std::sync::{Arc, Mutex};

fn main() {
    // —— 脚本化"云端"：按真实 Rodin v2 wire 形状回放响应 ——
    struct Cloud {
        log: Mutex<Vec<String>>,
        responses: Mutex<Vec<HttpResponse>>,
    }
    impl Transport for Cloud {
        fn send(&self, req: &HttpRequest) -> Result<HttpResponse, String> {
            self.log
                .lock()
                .unwrap()
                .push(format!("→ {:?} {}", req.method, short(&req.url)));
            self.responses
                .lock()
                .unwrap()
                .pop()
                .ok_or_else(|| "no scripted response".into())
        }
    }
    let cloud = Arc::new(Cloud {
        log: Mutex::new(Vec::new()),
        responses: Mutex::new(vec![
            // 5) CDN 签名 URL（免鉴权）→ GLB 字节
            HttpResponse::ok(oriera_genai::tiny_glb()),
            // 4) download：list[] 里按 .glb 找签名 URL
            HttpResponse::ok(
                r#"{"list":[{"name":"textures.zip","url":"https://cdn.hyper3d.com/t.zip"},
                    {"name":"model.glb","url":"https://cdn.hyper3d.com/m.glb"}]}"#
                    .as_bytes()
                    .to_vec(),
            ),
            // 3) status：全部 Done
            HttpResponse::ok(
                r#"{"jobs":[{"uuid":"j1","status":"Done"},{"uuid":"j2","status":"Done"}]}"#,
            ),
            // 2) status：Generating
            HttpResponse::ok(r#"{"jobs":[{"uuid":"j1","status":"Generating"}]}"#),
            // 1) create：201 顶层 uuid + jobs.subscription_key
            HttpResponse::ok(
                r#"{"message":"Submitted.","uuid":"9f2c…","jobs":{"uuids":["j1","j2"],"subscription_key":"sk-rodin-demo"},"consumed":0.5}"#,
            ),
        ]),
    });

    println!("══ OriEra × Hyper3D Rodin —— 文生 3D 全链演示 ══\n");
    println!("[1] 素材中心输入：\"一个黄铜咖啡壶，低多边形，手绘贴图风格\"");
    let provider = oriera_genai::create_generation_provider_with_transport(
        &ProviderConfig {
            kind: ProviderKind::Rodin,
            model: "rodin-gen-2".into(),
            api_key: "RODIN_API_KEY".into(),
            base_url: Some("https://api.hyper3d.com/api/v2".into()),
        },
        cloud.clone() as SharedTransport,
    );

    println!("[2] 创建任务（multipart + Bearer）…");
    let task = provider
        .request_generation(&GenerationRequest::text_to_3d(
            "npc_prop",
            "一个黄铜咖啡壶，低多边形，手绘贴图风格",
        ))
        .expect("create");
    println!("    ✓ 受理：TaskId = {task:?}");

    println!("[3] 轮询状态（subscription_key）…");
    match provider.poll_task(&task).expect("poll 1") {
        TaskStatus::Running => {
            println!("    ✓ Rodin: Generating —— 引擎侧 TaskStatus::Running（世界继续跑，不阻塞）")
        }
        _ => panic!("expected running"),
    }

    println!("[4] 第二次轮询：全部 Done → 下载清单取 .glb → 签名 URL 免鉴权即取");
    let asset = match provider.poll_task(&task).expect("poll 2") {
        TaskStatus::Succeeded(a) => a,
        _ => panic!("expected asset"),
    };
    println!(
        "    ✓ 资产落地：{} · {} bytes · provider={} · validate={:?}",
        asset.format.id(),
        asset.payload.len(),
        asset.provider_id,
        oriera_genai::validate_asset(&asset, &Default::default()).map(|_| "PASS")
    );

    println!("\n—— 云端实际收到的请求 ——");
    for l in cloud.log.lock().unwrap().iter() {
        println!("  {l}");
    }
    println!("\n结论：同一 GenerationProvider 契约，Tripo / Rodin 双供应商可互换。");
}

fn short(url: &str) -> &str {
    url.split("//").nth(1).unwrap_or(url)
}
