//! `cargo run -p oriera-agent --example cafe`
//!
//! The engine kernel runs a headless café: cognition, memory and the AI
//! director are plain `System<World>`s on the L2 scheduler. The ONLY
//! thing this application does is register components/resources, add
//! the systems, and read the `DialogueLog` to print. Swap `RuleProvider`
//! for an HTTP `ModelProvider` and the same loop talks to a live LLM —
//! no application code changes.

use oriera_agent::{
    CognitionSystem, DialogueLog, DirectorSystem, Episode, MemoryStore, Mood, Persona, WorldStory,
};
use oriera_core::Scheduler;
use oriera_ecs::World;
use oriera_infer::RuleProvider;
use std::sync::Arc;

fn main() {
    let mut world = World::new();
    world.insert_resource(DialogueLog::default());
    world.insert_resource(MemoryStore::new());
    world.insert_resource(WorldStory {
        day: 1,
        turn: 0,
        beats: vec![],
    });

    for (name, role, desc) in [
        (
            "老周",
            "咖啡馆老板 · 手冲派",
            "五十岁，开了十二年咖啡馆，记得每个熟客的口味，话不多但每句有分量。",
        ),
        (
            "Aria",
            "自由插画师 · 窗边常驻",
            "正在画一本'会说话的咖啡馆'绘本，感性，喜欢用颜色形容心情。",
        ),
        (
            "阿哲",
            "计算机系学生 · 想做游戏",
            "在写一个世界引擎 demo，好奇话多容易兴奋，向老周学手冲。",
        ),
    ] {
        let e = world.spawn();
        world.insert(
            e,
            Persona {
                name: name.into(),
                role: role.into(),
                description: desc.into(),
            },
        );
        world.insert(e, Mood::Neutral);
    }

    // 初始记忆：世界不是从空白开始的
    {
        let mem = world.resource_mut::<MemoryStore>().unwrap();
        mem.record(
            "老周",
            Episode {
                turn: 0,
                day: 1,
                about: "Aria".into(),
                text: "Aria 总在窗边画到打烊，喜欢双份浓缩".into(),
                importance: 4,
            },
        );
        mem.record(
            "阿哲",
            Episode {
                turn: 0,
                day: 1,
                about: "老周".into(),
                text: "想跟老周学手冲，顺便蹭插座写代码".into(),
                importance: 3,
            },
        );
    }

    let provider = Arc::new(RuleProvider::new());
    let mut sched: Scheduler<World> = Scheduler::new();
    // 引擎系统注册：认知每 3 tick 说一句话，导演每 10 tick 推进世界
    sched.add_system(Box::new(CognitionSystem::new(provider.clone(), 3)));
    sched.add_system(Box::new(DirectorSystem::new(provider, 10)));

    println!("╔══════════════════════════════════════════════╗");
    println!("║  隅角咖啡馆 · OriEra 引擎内核（headless）     ║");
    println!("╚══════════════════════════════════════════════╝");
    println!("L7 provider = rule-v1（换 HTTP ModelProvider 即接真模型）\n");

    let mut last_len = 0;
    let mut last_beats = 0;
    for _ in 0..40 {
        sched.tick(&mut world);
        let log = world.resource::<DialogueLog>().unwrap();
        for t in &log.turns[last_len..] {
            println!("  {} → {}：{}", t.speaker, t.to, t.text);
        }
        last_len = log.turns.len();
        let story = world.resource::<WorldStory>().unwrap();
        for b in &story.beats[last_beats..] {
            println!("  🎭 [AI导演·第{}日] {}", story.day, b);
        }
        last_beats = story.beats.len();
    }

    println!("\n════════ 内核状态 ════════");
    let (turn, day) = {
        let s = world.resource::<WorldStory>().unwrap();
        (s.turn, s.day)
    };
    println!(
        "  回合 {} · 世界日 {} · 对话 {} 条",
        turn,
        day,
        world.resource::<DialogueLog>().unwrap().turns.len()
    );
    println!(
        "  长期记忆 {} 条（跨会话持久化由平台壳负责）",
        world.resource::<MemoryStore>().unwrap().len_total()
    );
    let mem = world.resource::<MemoryStore>().unwrap();
    for hit in mem.retrieve("老周", "Aria", "口味 浓缩", 40, 2) {
        println!("  老周的记忆 → {}", hit.text);
    }
    println!("\n✅ NPC 对话 = 引擎系统；应用只注册组件、读资源。这就是 AI 内嵌于引擎架构。");
}
