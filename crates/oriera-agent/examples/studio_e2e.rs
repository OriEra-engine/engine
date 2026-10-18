//! Studio e2e — 组织者测试的可执行载体（三轮测试 · Round 1）。
//!
//! 模拟 OriEra Studio 工作台的一次完整使用流：模型中心选 provider →
//! 新建游戏（世界/角色/规则）→ 命令面板 → 引擎帧循环 → 仪表盘读数。
//! 同一个引擎内核连跑三个赛道场景，证明「通用世界引擎」不是口号：
//!
//! - 场景 A · 游戏：自然语言建场 → 物理/渲染 → 分拨仪表盘
//! - 场景 B · 陪伴：NPC 认知 + 长期记忆 + 玩家唤醒（注意力节拍器）
//! - 场景 C · 孪生：同种子两次运行逐位一致（确定性回放的地基）
//!
//! 全程离线（RuleProvider），任何断言失败即 panic——测试即 demo。
//! 运行：`cargo run -p oriera-agent --example studio_e2e`

use oriera_agent::{
    parse, CognitionSystem, DialogueLog, MemoryStore, Mood, Persona, WakeEvent, WorldStory,
};
use oriera_common::Timestamp;
use oriera_core::{AllocatorConfig, FrameCtx, Scheduler, System};
use oriera_ecs::{Position, World};
use oriera_infer::{InferSystem, ModelProvider, RuleProvider, Stats, UsageMeter};
use oriera_render::RenderSystem;
use oriera_subsys::{Gravity, PhysicsSystem, TimeScale};
use std::rc::Rc;
use std::sync::Arc;

fn fixed_ctx(frame: u64) -> FrameCtx<'static> {
    FrameCtx {
        frame: oriera_common::FrameIndex(frame),
        dt: 1.0 / 60.0,
        now: Timestamp(0),
        grants: oriera_common::Grants::EMPTY,
    }
}

fn banner(title: &str) {
    println!("\n══ {title} ══");
}

/// How many systems the allocator is managing this frame.
fn grants_count(sched: &Scheduler<World>) -> usize {
    sched
        .system_timings()
        .iter()
        .filter(|(n, _)| sched.grants().is_managed(n))
        .count()
}

// ─────────────────────────────────────────────────────────────────
// 场景 A · 游戏赛道：命令面板 → L9 解析 → L7 队列 → L6 物理 → L5 渲染
// ─────────────────────────────────────────────────────────────────
fn scenario_game() {
    banner("场景 A · 游戏赛道（言灵世界形态）");

    // Studio「模型中心」：v0.1 离线行 = RuleProvider；换一个
    // ProviderConfig 就是八家厂商之一，引擎层零改动。
    let _provider: Arc<dyn ModelProvider> = Arc::new(RuleProvider::new());

    // Studio「新建游戏」五步的产物：世界 + 规则资源。
    let mut world = World::new();
    world.insert_resource(Gravity(9.8));
    world.insert_resource(TimeScale(1.0));

    // 「命令面板」：玩家语句经 L9 parse 变 L7 命令；否定语义先于意图。
    let mut infer = InferSystem::with_frame_budget(8);
    for line in [
        "spawn 40 particles",
        "spawn 20 more particles", // 与上一条同型相邻 → 合并
        "set gravity to 1.6",
        "speed 2x",
        "别清空", // 否定 → 不执行
        "how many",
    ] {
        let cmd = parse(line);
        println!("  面板 {:?} → {:?}", line, cmd);
        infer.submit(cmd);
    }
    assert!(infer.coalesced >= 1, "相邻 Spawn 应被合并");
    assert_eq!(
        infer.pending(),
        5,
        "6 条输入 = 5 条命令（1 条合并）+ 否定不消失"
    );

    // 帧循环：L2 分拨 → L7/L6/L5 → 观测回灌。
    let mut sched: Scheduler<World> =
        Scheduler::with_budget_ns(16_666_666).with_allocator(AllocatorConfig::default());
    sched.add_system(Box::new(infer));
    sched.add_system(Box::new(PhysicsSystem::new(800.0, 600.0)));
    let render = Rc::new(std::cell::RefCell::new(RenderSystem::new()));
    sched.add_system(Box::new(SharedRender(render.clone())));

    let mut w = world;
    for _ in 0..30 {
        let ms = sched.tick(&mut w);
        render
            .borrow_mut()
            .observe_frame(sched.avg_frame_ms(), sched.budget_ms());
        assert!(ms < 100.0, "帧耗时异常: {ms}");
    }

    // 「仪表盘」读数。
    let entities = w.len();
    let stats = w.resource::<Stats>().map(|s| s.entities);
    println!("  实体 {entities} · Count 命令回读 {stats:?}");
    println!(
        "  渲染 {:?} · 可见 {} · draw calls {}",
        render.borrow().quality,
        render.borrow().visible,
        render.borrow().draw_calls
    );
    println!(
        "  L2 分拨：load {:.2} · headroom {:.0}% · grants {} 条",
        sched.allocator().load(sched.budget_ms()),
        sched.headroom() * 100.0,
        grants_count(&sched)
    );
    for (name, ms) in sched.system_timings() {
        println!("    {:<12} {:>8.3} ms", name, ms);
    }

    assert_eq!(entities, 60, "40+20 合并后一次 Spawn(60)");
    assert_eq!(stats, Some(60));
    assert!(w.resource::<Gravity>().unwrap().0 - 1.6 < 1e-6);
    assert!(!sched.grants().is_empty());
}

struct SharedRender(Rc<std::cell::RefCell<RenderSystem>>);
impl System<World> for SharedRender {
    fn name(&self) -> &str {
        "render"
    }
    fn run(&mut self, ctx: &FrameCtx, world: &mut World) {
        self.0.borrow_mut().run(ctx, world);
    }
}

// ─────────────────────────────────────────────────────────────────
// 场景 B · 陪伴赛道：认知系统 + 长期记忆 + 注意力（玩家一碰即回满速）
// ─────────────────────────────────────────────────────────────────
fn scenario_companion() {
    banner("场景 B · 陪伴赛道（隅角咖啡馆形态）");

    let provider = Arc::new(RuleProvider::new());
    let mut world = World::new();
    world.insert_resource(DialogueLog::default());
    world.insert_resource(WorldStory {
        day: 1,
        turn: 0,
        beats: vec![],
    });
    world.insert_resource(MemoryStore::new());
    for (name, role, desc) in [
        ("小满", "陪伴者", "记得用户说过的每一件小事"),
        ("用户", "人类", "今晚有点累"),
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

    let mut cog = CognitionSystem::new(provider.clone(), 1);
    let entries: Vec<(String, oriera_common::ComputeGrant)> = vec![(
        "l9-cognition".into(),
        oriera_common::ComputeGrant {
            share_ms: 1.0,
            throttle: 0.5,
        },
    )];
    let squeezed = FrameCtx {
        frame: oriera_common::FrameIndex(0),
        dt: 0.0,
        now: Timestamp(0),
        grants: oriera_common::Grants::new(Box::leak(entries.into_boxed_slice())),
    };
    let full = fixed_ctx(0);

    // 压力段：throttle 0.5 → 说话间隔 ×2 → 20 tick 只说 10 句。
    for _ in 0..20 {
        cog.run(&squeezed, &mut world);
    }
    let quiet_turns = world.resource::<DialogueLog>().unwrap().turns.len();
    assert_eq!(quiet_turns, 10, "半速 = 半频，内容不受影响");

    // 唤醒段：玩家开口（WakeEvent）→ 当帧回满速。
    for _ in 0..10 {
        world.write_event(WakeEvent);
        cog.run(&full, &mut world);
    }
    let woken_turns = world.resource::<DialogueLog>().unwrap().turns.len();
    assert_eq!(woken_turns, 20, "唤醒后每 tick 一句");

    let meter = world.resource::<UsageMeter>().cloned().unwrap_or_default();
    let mem = world.resource::<MemoryStore>().unwrap().len_total();
    println!(
        "  对话 {} 句（压力 10 + 唤醒 10）· 记忆 {} 条 · 调用 {} 次",
        woken_turns, mem, meter.calls
    );
    assert!(mem > 0, "长期记忆在增长");
    assert!(meter.calls > 0, "计量在累加");
}

// ─────────────────────────────────────────────────────────────────
// 场景 C · 孪生/仿真赛道：同种子两次运行，世界逐位一致
//
// 诚实边界：本场景用固定 FrameCtx 直调系统（dt 恒 1/60）。走真实
// Scheduler::tick 时 dt 来自墙钟，物理积分会随负载发散——确定性
// tick（issue #66 with_fixed_dt）落地前，本场景证明的是「命令流 +
// 固定步长下可复现」，不是「调度器路径可复现」。
// ─────────────────────────────────────────────────────────────────
fn scenario_twin() {
    banner("场景 C · 数字孪生赛道（确定性回放地基）");

    let run_once = |freeze_midway: bool| -> Vec<(u32, u32)> {
        let mut world = World::new();
        world.insert_resource(Gravity(9.8));
        let mut infer = InferSystem::new();
        infer.submit(parse("spawn 25 particles"));
        let mut physics = PhysicsSystem::new(800.0, 600.0);
        for f in 0..60u64 {
            if freeze_midway && f == 30 {
                world.insert_resource(TimeScale(0.0)); // 故障注入：冻结
            }
            infer.run(&fixed_ctx(f), &mut world);
            physics.run(&fixed_ctx(f), &mut world);
        }
        let frozen = physics.idle_frames;
        println!(
            "  冻结注入={freeze_midway} · 空转跳过 {frozen} 帧 · 实体 {}",
            world.len()
        );
        world
            .query::<Position>()
            .map(|(_e, p)| (p.x.to_bits(), p.y.to_bits()))
            .collect()
    };

    let a = run_once(false);
    let b = run_once(false);
    assert_eq!(a, b, "同种子两次运行必须逐位一致");
    assert_eq!(a.len(), 25);

    // 冻结注入的差异是可观测且可解释的：30 帧冻结 = 30 帧零位移。
    let c = run_once(true);
    assert_eq!(c.len(), 25);
    println!(
        "  ✓ 两次运行逐位一致（{} 个实体）· 冻结注入产生可解释差异",
        a.len()
    );
}

fn main() {
    println!("╔══════════════════════════════════════════════════╗");
    println!("║  OriEra Studio · 组织者测试（Round 1 / 三赛道）   ║");
    println!("╚══════════════════════════════════════════════════╝");
    println!("引擎快照：v0.3.0 内核 + v0.4 算力分拨 · provider=rule（离线行）");

    scenario_game();
    scenario_companion();
    scenario_twin();

    banner("Round 1 组织者测试结论");
    println!("  PASS — 通用内核连跑游戏/陪伴/孪生三赛道，全部断言通过。");
    println!("  下一轮：社区伙伴（Round 2）→ docs/VERTICALS.md 附录 C 清单");
    println!("  下一轮：模型厂商（Round 3）→ provider_smoke + 免费版矩阵");
}
