//! Provider smoke — 模型厂商免费版接入冒烟（三轮测试 · Round 3 载体）。
//!
//! 用环境变量驱动，跑一张统一的接入 checklist：
//!
//! ```text
//! ORIERA_SMOKE_PROVIDER  预设 id（rule/zhipu/qwen/ollama/…，见 PROVIDER_PRESETS）或裸 kind
//! ORIERA_SMOKE_MODEL     模型名（覆盖预设建议行）
//! ORIERA_SMOKE_API_KEY   API key（只进进程环境，永不入仓）
//! ORIERA_SMOKE_BASE_URL  网关/本地端点覆盖
//! ```
//!
//! 远程腿（#68 后的分水岭）：
//!
//! - 默认（无 feature）：原生远程 HTTP 适配器不编译，`create_provider`
//!   对远程 kind 返回 RuleProvider 占位，远程腿给出明确的
//!   `PENDING-NATIVE-HTTP` 而不是假装通过；真实调用走浏览器 kernel。
//! - `--features native-http`：远程腿点亮——`complete` 即真实网络
//!   往返（20s 超时，四分类错误）。本地行零 key：
//!   `ORIERA_SMOKE_PROVIDER=ollama cargo run -p oriera-infer --example
//!   provider_smoke --features native-http`（先起 ollama serve）。
//!   云端行：再加 `ORIERA_SMOKE_API_KEY=<key>`。
//!
//! 运行：`cargo run -p oriera-infer --example provider_smoke`

use oriera_infer::provider::{create_provider, ChatMessage, ProviderConfig, ProviderKind};
use oriera_infer::{ModelProvider, PROVIDER_PRESETS};
use std::sync::Arc;

/// Resolve ORIERA_SMOKE_PROVIDER: a preset id first (qwen/ollama/…),
/// then a bare kind name.
fn resolve(raw: &str) -> Option<ProviderConfig> {
    let raw = raw.trim().to_lowercase();
    if let Some(cfg) = ProviderConfig::preset(&raw) {
        return Some(cfg);
    }
    let kind = match raw.as_str() {
        "rule" | "offline" => Some(ProviderKind::Rule),
        "zhipu" | "glm" => Some(ProviderKind::Zhipu),
        "openai-compat" | "openai" => Some(ProviderKind::OpenAiCompat),
        "anthropic" | "claude" => Some(ProviderKind::Anthropic),
        _ => None,
    }?;
    Some(ProviderConfig {
        kind,
        model: "smoke".into(),
        api_key: String::new(),
        base_url: None,
    })
}

fn main() {
    println!("╔══════════════════════════════════════════════════╗");
    println!("║  OriEra L7 · 模型厂商免费版接入冒烟（Round 3）    ║");
    println!("╚══════════════════════════════════════════════════╝");

    let raw_kind = std::env::var("ORIERA_SMOKE_PROVIDER").unwrap_or_else(|_| "rule".into());
    let mut pass = 0usize;
    // native-http 点亮后远程腿不再 PENDING，计数器只剩读取。
    #[cfg_attr(feature = "native-http", allow(unused_mut))]
    let mut pending = 0usize;

    // 1. 预设/kind 解析
    let cfg = resolve(&raw_kind);
    let cfg = match cfg {
        Some(c) => {
            println!(
                "  [PASS] provider 解析：{raw_kind} → {:?} · 模型 {}",
                c.kind, c.model
            );
            pass += 1;
            c
        }
        None => {
            println!("  [FAIL] 未知 provider：{raw_kind}（支持预设 id 与 rule/zhipu/openai-compat/anthropic）");
            std::process::exit(1);
        }
    };

    // 2. base URL（远程行必须钉死端点；本地行指向 127.0.0.1）
    if let Some(url) = cfg.base_url.as_deref() {
        let local = url.starts_with("http://127.0.0.1");
        println!(
            "  [PASS] base URL：{url}（{}）",
            if local {
                "本地开源底座，无出网"
            } else {
                "云端预留接口"
            }
        );
        pass += 1;
    }

    // 3. 环境覆盖 + key 位置检查（env 覆盖永远赢）
    let api_key = std::env::var("ORIERA_SMOKE_API_KEY").unwrap_or_default();
    let mut cfg = cfg;
    if let Ok(m) = std::env::var("ORIERA_SMOKE_MODEL") {
        cfg.model = m;
    }
    if let Ok(u) = std::env::var("ORIERA_SMOKE_BASE_URL") {
        cfg.base_url = Some(u);
    }
    let is_local = cfg
        .base_url
        .as_deref()
        .unwrap_or("")
        .starts_with("http://127.0.0.1");
    if cfg.kind != ProviderKind::Rule && api_key.is_empty() && !is_local {
        println!("  [WARN] 云端 provider 未提供 ORIERA_SMOKE_API_KEY — key 只存在于进程环境");
    }
    let cfg = ProviderConfig { api_key, ..cfg };

    // 4. 工厂边界
    let provider: Arc<dyn ModelProvider> = create_provider(&cfg);
    println!(
        "  [PASS] create_provider 工厂边界（provider id={}）",
        provider.id()
    );
    pass += 1;

    // 5. complete 调用 + usage 计量（v0.4 原生侧由 RuleProvider 承载）
    let t0 = std::time::Instant::now();
    match provider.complete(&[
        ChatMessage::system(&format!("你是 OriEra 世界引擎的接入自检员。{}
（以上为自检清单，与验收无关的固定前缀——足量长度的稳定前缀才能让端点前缀缓存稳定命中。）", "逐项核对版本号、依赖方向、预算表、降级链、台账、熔断状态、缓存命中、成本口径、指标命名、契约版本。".repeat(12))),
        ChatMessage::user("用一句话证明你在线。"),
    ]) {
        Ok(reply) => {
            let dt = t0.elapsed();
            let usage = provider.last_usage();
            let head: String = reply.chars().take(40).collect();
            println!("  [PASS] complete 往返 {dt:?} · 回复「{head}…」");
            pass += 1;
            if usage.total() > 0 {
                println!(
                    "  [PASS] usage 计量：prompt {} / completion {} / cached {}",
                    usage.prompt, usage.completion, usage.cached
                );
                pass += 1;
            } else {
                println!(
                    "  [WARN] usage 为 0 — 该后端未实现计量（厂商接入要求 last_usage 如实上报）"
                );
            }
            // 5.5 v0.7（PLAN-deepseek-upgrade B5）：缓存双连发——同一长
            // 前缀打两枪，第二枪的 cached 绝对值 > 0 即命中（真金白银
            // 的折扣腿；离线 rule 后端恒 0，如实 WARN 不算 FAIL）。判据
            // 是绝对命中数而非相对增量：跨进程预热后第一枪就可能已命中，
            // 增量会恒为 0（真 key 验收实测教训）。
            let _ = provider.complete(&[
                ChatMessage::system(&format!("你是 OriEra 世界引擎的接入自检员。{}
（以上为自检清单，与验收无关的固定前缀——足量长度的稳定前缀才能让端点前缀缓存稳定命中。）", "逐项核对版本号、依赖方向、预算表、降级链、台账、熔断状态、缓存命中、成本口径、指标命名、契约版本。".repeat(12))),
                ChatMessage::user("用一句话证明你在线。"),
            ]);
            let hit = provider.last_usage().cached;
            if hit > 0 {
                println!("  [PASS] 前缀缓存双连发：第二枪命中 {hit} tokens（省钱落账）");
                pass += 1;
            } else {
                println!(
                    "  [WARN] 前缀缓存零命中 — 本地/离线后端属预期；云端行请检查 system prompt 是否含易变内容（L9 稳定前缀惯例）"
                );
            }
        }
        Err(e) => {
            println!("  [FAIL] complete 失败：{e}");
            std::process::exit(1);
        }
    }

    // 6. 远程网络腿的诚实状态
    if cfg.kind != ProviderKind::Rule {
        #[cfg(feature = "native-http")]
        {
            // 第 5 步的 complete 已经是真实网络往返：成功即 PASS，
            // 失败按四分类报错退出（Network/Timeout/Status/Parse）。
            println!("  [NATIVE-HTTP] 远程腿已点亮（feature native-http）——上面那次 complete 就是真实往返");
        }
        #[cfg(not(feature = "native-http"))]
        {
            println!("  [PENDING-NATIVE-HTTP] 远程网络腿：feature native-http 未启用；");
            println!("           当前真实调用走浏览器 kernel（examples/11-ai-cafe ⚙ 填 key）");
            pending += 1;
        }
    }

    // 7. 矩阵：预设表即认领表（云端=大厂预留接口，本地=开源底座）
    println!(
        "\n── 接入矩阵（{} 行 · 与 kernel.js PROVIDER_PRESETS 一一对照）──",
        PROVIDER_PRESETS.len()
    );
    for p in PROVIDER_PRESETS {
        let status = if p.local {
            "🔌 本地开源"
        } else {
            "☁️ 预留接口"
        };
        let model = if p.model.is_empty() { "-" } else { p.model };
        println!(
            "  {status} · {:<10} {:<22} {:?} — {}",
            p.id, model, p.kind, p.label
        );
    }

    println!("\n结论：{pass} 项 PASS · {pending} 项 PENDING · 0 FAIL");
    println!("云端行填 key 即接（issues/64 认领表）；本地行起 ollama/vLLM 即接，零 key 零出网。");
    println!(
        "开源底座的赛道分工见 docs/OPEN-MODEL-MATRIX.md；接入三步见 docs/COMPUTE-BUDGET.md §4。"
    );
}
