//! RFC-3 工具调用引擎闭环示例(离线,规则后端)。
//!
//! ```bash
//! cargo run -p oriera-agent --example tool_loop --features json
//! ```
//! 全程:词表 → 模型(规则后端用 `spawn {"count":N}` 标记模拟工具
//! 请求)→ 引擎派发 → 命令下一帧落地 → 世界改变 → 把结果讲回对话。
//! 换成真厂商模型(HttpProvider + native-http)时同一循环直接可用。

use oriera_agent::tools::EngineTools;
use oriera_core::{FrameCtx, System};
use oriera_ecs::World;
use oriera_infer::{InferSystem, ModelProvider, RuleProvider};
use std::sync::Arc;

fn ctx() -> FrameCtx<'static> {
    use oriera_common::{FrameIndex, Grants, Timestamp};
    FrameCtx {
        frame: FrameIndex(0),
        dt: 0.0,
        now: Timestamp(0),
        grants: Grants::EMPTY,
    }
}

fn main() {
    let provider = Arc::new(RuleProvider::new());
    let tools = EngineTools::definitions();
    println!("🔧 引擎工具词表({} 个):", tools.len());
    for t in &tools {
        println!("   · {} — {}", t.name, t.description);
    }
    assert!(provider.supports_tools(), "规则后端支持工具模拟");

    let mut infer = InferSystem::new();
    let mut world = World::new();

    // 一段"导演式"输入:模型看到词表后决定调工具
    let user_text = "推进演出:spawn {\"count\":12},再看看情况。";
    println!("\n🧑 玩家:{user_text}");

    // run_tool_agent:完成 → 调工具 → 结算一帧 → 结果回话 → 再完成,
    // 直到模型自己决定"说完了" —— AI 自主调控的运行时形态。
    let report = oriera_agent::tools::run_tool_agent(
        &*provider,
        user_text,
        6,
        &mut infer,
        &mut world,
        &ctx(),
    )
    .expect("agent loop converges");
    println!("🤖 导演汇报:{report}");
    println!();
    println!("🌍 世界实体数:{}", world.len());

    // 顺手读一次世界状态,证明循环的每一步都真实落地
    infer.submit(oriera_infer::Command::Count);
    infer.run(&ctx(), &mut world);
    let stats = world.resource::<oriera_infer::Stats>().copied();
    println!("📊 Stats 资源:{stats:?}");
    println!();
    println!("✅ 多轮工具代理完成:模型自主决定调什么、看结果、何时收口。");
}
