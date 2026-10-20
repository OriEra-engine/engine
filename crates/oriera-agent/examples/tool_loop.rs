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
use oriera_infer::{ChatMessage, InferSystem, ModelProvider, RuleProvider, ToolOutcome};
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

    let outcome = provider
        .complete_with_tools(
            &[
                ChatMessage::system("你是世界导演,可以通过给出的工具操作世界。"),
                ChatMessage::user(user_text),
            ],
            &tools,
        )
        .expect("tool-enabled completion");

    match outcome {
        ToolOutcome::ToolCalls(calls) => {
            for call in &calls {
                println!("🤖 模型请求工具:{} args={}", call.name, call.arguments_json);
                let ack = EngineTools::dispatch(call, &mut infer);
                println!("⚙️  引擎:{ack}");
            }
        }
        ToolOutcome::Text(t) => println!("🤖 模型(纯文本):{t}"),
    }

    // 命令在下一帧落地 —— AI 的决定真正改变了世界
    infer.run(&ctx(), &mut world);
    println!("\n🌍 世界实体数:{}", world.len());

    // 结果回话:统计一次,把世界状态讲回对话
    let outcome = provider.complete_with_tools(
        &[ChatMessage::user(format!(
            "现在世界里有多少实体?执行 count_entities {{}} 查一下。(当前 {})",
            world.len()
        ))],
        &tools,
    );
    if let Ok(ToolOutcome::ToolCalls(calls)) = outcome {
        for call in &calls {
            let ack = EngineTools::dispatch(call, &mut infer);
            println!("⚙️  引擎:{ack}");
        }
    }
    infer.run(&ctx(), &mut world);
    let stats = world.resource::<oriera_infer::Stats>().copied();
    println!("📊 Stats 资源:{stats:?}");
    println!("\n✅ 工具闭环完成:模型的决定 → 引擎命令 → 世界改变 → 状态可回读。");
}
