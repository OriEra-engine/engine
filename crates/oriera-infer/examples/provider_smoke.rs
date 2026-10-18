//! Provider smoke — 模型厂商免费版接入冒烟（三轮测试 · Round 3 载体）。
//!
//! 用环境变量驱动，跑一张统一的接入 checklist：
//!
//! ```text
//! ORIERA_SMOKE_PROVIDER  rule | zhipu | openai-compat | anthropic   （默认 rule）
//! ORIERA_SMOKE_MODEL     模型名（如 glm-4-flash）
//! ORIERA_SMOKE_API_KEY   API key（只进进程环境，永不入仓）
//! ORIERA_SMOKE_BASE_URL  网关/本地端点覆盖（如 http://127.0.0.1:11434/v1）
//! ```
//!
//! 诚实边界（截至 v0.4）：原生侧的远程 HTTP 适配器尚未落地
//! （`create_provider` 对远程 kind 返回 RuleProvider 占位），真实网络腿
//! 由浏览器 kernel（demo 11 ⚙ 填 key）承载。因此本 harness 在原生侧
//! 验证的是**接入链路的全部本地环节**——kind 解析、默认 base URL、
//! 工厂边界、complete 调用、usage 计量——远程腿给出明确的
//! `PENDING-NATIVE-HTTP` 而不是假装通过。
//!
//! 运行：`cargo run -p oriera-infer --example provider_smoke`

use oriera_infer::provider::{create_provider, ChatMessage, ProviderConfig, ProviderKind};
use oriera_infer::ModelProvider;
use std::sync::Arc;

/// 各厂商的免费/低价行 + 原生配置形态。厂商认领后在此表打 ✅。
const FREE_TIER_MATRIX: &[(&str, &str, ProviderKind, &str)] = &[
    // (厂商, 免费模型, kind, 备注)
    (
        "offline",
        "rule-v1",
        ProviderKind::Rule,
        "零依赖离线行，CI 与无 key 环境",
    ),
    (
        "zhipu",
        "glm-4-flash",
        ProviderKind::Zhipu,
        "开放平台免费档，国内直连",
    ),
    (
        "deepseek",
        "deepseek-chat",
        ProviderKind::OpenAiCompat,
        "OpenAI 兼容端点",
    ),
    (
        "moonshot",
        "kimi-k2-0905-preview",
        ProviderKind::OpenAiCompat,
        "OpenAI 兼容端点",
    ),
    (
        "groq",
        "llama-3.1-8b-instant",
        ProviderKind::OpenAiCompat,
        "免费档，低延迟",
    ),
    (
        "openrouter",
        "<any>:free",
        ProviderKind::OpenAiCompat,
        ":free 后缀模型",
    ),
    (
        "ollama",
        "qwen2.5:7b",
        ProviderKind::OpenAiCompat,
        "本地端点，BASE_URL 覆盖",
    ),
    (
        "anthropic",
        "claude-haiku",
        ProviderKind::Anthropic,
        "messages API",
    ),
];

fn parse_kind(raw: &str) -> Option<ProviderKind> {
    match raw.trim().to_lowercase().as_str() {
        "rule" | "offline" => Some(ProviderKind::Rule),
        "zhipu" | "glm" => Some(ProviderKind::Zhipu),
        "openai-compat" | "openai" | "deepseek" | "moonshot" | "groq" | "openrouter" | "ollama"
        | "vllm" => Some(ProviderKind::OpenAiCompat),
        "anthropic" | "claude" => Some(ProviderKind::Anthropic),
        _ => None,
    }
}

fn main() {
    println!("╔══════════════════════════════════════════════════╗");
    println!("║  OriEra L7 · 模型厂商免费版接入冒烟（Round 3）    ║");
    println!("╚══════════════════════════════════════════════════╝");

    let raw_kind = std::env::var("ORIERA_SMOKE_PROVIDER").unwrap_or_else(|_| "rule".into());
    let model = std::env::var("ORIERA_SMOKE_MODEL").unwrap_or_else(|_| "rule-v1".into());
    let api_key = std::env::var("ORIERA_SMOKE_API_KEY").unwrap_or_default();
    let base_url = std::env::var("ORIERA_SMOKE_BASE_URL").ok();

    let mut pass = 0usize;
    let mut pending = 0usize;

    // 1. kind 解析
    let kind = parse_kind(&raw_kind);
    match kind {
        Some(k) => {
            println!("  [PASS] provider kind 解析：{raw_kind} → {k:?}");
            pass += 1;
            // 2. 默认 base URL 解析（远程行）
            if k != ProviderKind::Rule {
                println!("  [PASS] 默认 base URL：{}", k.default_base_url());
                pass += 1;
            }
        }
        None => {
            println!(
                "  [FAIL] 未知 provider：{raw_kind}（支持 rule/zhipu/openai-compat/anthropic）"
            );
            std::process::exit(1);
        }
    }
    let kind = kind.unwrap();

    // 3. 工厂边界 + key 位置检查
    if kind != ProviderKind::Rule && api_key.is_empty() {
        println!("  [WARN] 远程 provider 未提供 ORIERA_SMOKE_API_KEY — key 只存在于进程环境");
    }
    let cfg = ProviderConfig {
        kind,
        model: model.clone(),
        api_key,
        base_url,
    };
    let provider: Arc<dyn ModelProvider> = create_provider(&cfg);
    println!(
        "  [PASS] create_provider 工厂边界（provider id={}）",
        provider.id()
    );
    pass += 1;

    // 4. complete 调用 + usage 计量（v0.4 原生侧由 RuleProvider 承载）
    let t0 = std::time::Instant::now();
    match provider.complete(&[
        ChatMessage::system("你是 OriEra 世界引擎的接入自检员。"),
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
                    "  [PASS] usage 计量：prompt {} / completion {}",
                    usage.prompt, usage.completion
                );
                pass += 1;
            } else {
                println!(
                    "  [WARN] usage 为 0 — 该后端未实现计量（厂商接入要求 last_usage 如实上报）"
                );
            }
        }
        Err(e) => {
            println!("  [FAIL] complete 失败：{e}");
            std::process::exit(1);
        }
    }

    // 5. 远程网络腿的诚实状态
    if kind != ProviderKind::Rule {
        println!("  [PENDING-NATIVE-HTTP] 远程网络腿：原生 HTTP 适配器 v0.5 落地；");
        println!("           当前真实调用走浏览器 kernel（examples/11-ai-cafe ⚙ 填 key）");
        pending += 1;
    }

    println!("\n── 免费版矩阵（厂商认领后在此打 ✅）──");
    for (vendor, free_model, k, note) in FREE_TIER_MATRIX {
        let status = if *k == ProviderKind::Rule {
            "✅ 离线行"
        } else {
            "⬜ 待认领"
        };
        println!(
            "  {status} · {vendor:<11} {free_model:<24} {:?} — {note}",
            k
        );
    }

    println!("\n结论：{pass} 项 PASS · {pending} 项 PENDING · 0 FAIL");
    println!("厂商接入三步：实现 ModelProvider → 壳层 ProviderConfig（key 不入仓）→ 挂调度器");
    println!("详见 docs/COMPUTE-BUDGET.md §4 与 docs/STUDIO-PIPELINE.md P2 管线。");
}
