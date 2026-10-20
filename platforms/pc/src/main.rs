//! PC entry point — full engine boot loop.
//!
//! Boot order follows the layer stack (bottom-up), then the loop runs
//! top-down through the L2 scheduler: L9 agent parses a chat line into a
//! command, L7 executes it on the world, L6 steps physics, L5 submits a
//! frame. The world is owned HERE — by the application shell — and passed
//! through the scheduler; no layer reaches into a global.

use oriera_agent::parse;
use oriera_asset::AssetStore;
use oriera_core::Scheduler;
use oriera_ecs::{Position, Velocity, World};
use oriera_infer::{CostMeter, InferSystem, Stats, UsageMeter};
use oriera_render::RenderSystem;
use oriera_subsys::{Gravity, PhysicsSystem, TimeScale};

fn main() {
    tracing_subscriber::fmt::init();

    // ── L1 PAL: detect platform ──
    let profile = oriera_pal::detect_platform();
    let banner = format!("OriEra World Engine v{} (PC)", env!("CARGO_PKG_VERSION"));
    println!("╔══════════════════════════════════════╗");
    println!("║{banner:^38}║");
    println!("╚══════════════════════════════════════╝");
    println!("  Platform : {:?} / {}", profile.family, profile.os_name);
    println!("  Budget   : {} µs/frame", profile.frame_budget_us);
    println!("  Caps     : {:?}", profile.caps);

    // ── L3 ECS: the world + shared resources ──
    let mut world = World::new();
    world.insert_resource(Gravity(9.8));
    for i in 0..100 {
        let e = world.spawn();
        world.insert(
            e,
            Position {
                x: (i as f32 * 10.0) % 800.0,
                y: (i as f32 * 7.0) % 600.0,
            },
        );
        world.insert(
            e,
            Velocity {
                vx: (i as f32 - 50.0) * 0.1,
                vy: (i as f32 - 50.0) * 0.05,
            },
        );
    }
    println!("  ECS      : {} entities, Gravity(9.8)", world.len());

    // ── L4 Asset: named load + hot swap (the L8 landing pad) ──
    #[derive(Debug)]
    struct Texture {
        width: u32,
        height: u32,
    }
    let mut assets = AssetStore::new();
    let tex = assets.insert_named(
        "hero",
        Texture {
            width: 512,
            height: 512,
        },
    );
    let loaded = assets.get_named::<Texture>("hero").expect("just loaded");
    println!(
        "  Assets   : 'hero' {}×{} ({:?})",
        loaded.width, loaded.height, tex
    );
    assets.insert_named(
        "hero",
        Texture {
            width: 1024,
            height: 1024,
        }, // regenerated in place — same name, same handle
    );
    let swapped = assets.get_named::<Texture>("hero").unwrap();
    println!(
        "  Hot swap : 'hero' → {}×{} (handle {:?} unchanged)",
        swapped.width, swapped.height, tex
    );

    // ── L9 Agent → L7 Infer: chat lines become engine commands ──
    // The agent parses intent; the inference runtime executes it on the
    // world next frame.
    let mut infer = InferSystem::new();
    for line in [
        "spawn 50 more particles",
        "set gravity to 1.6",
        "speed 2x",
        "teleport to 10 -20",
        "how many",
    ] {
        let cmd = parse(line);
        println!("  Agent    : {:?} → {:?}", line, cmd);
        infer.submit(cmd);
    }

    // ── L2 Core: scheduler graded on the platform's own budget ──
    let mut sched: Scheduler<World> =
        Scheduler::with_budget_ns(profile.frame_budget_us as u64 * 1000);
    sched.add_system(Box::new(infer));
    sched.add_system(Box::new(PhysicsSystem::new(800.0, 600.0)));
    // Shared render handle: the scheduler runs it, the main loop feeds
    // its adaptive quality governor (L5) with the L2 frame stats.
    let render = std::rc::Rc::new(std::cell::RefCell::new(RenderSystem::new()));
    sched.add_system(Box::new(SharedRender(render.clone())));

    // ── Run the loop ──
    println!();
    println!("  Running 10 frames...");
    for f in 0..10 {
        // Slow the world to half pace mid-run: pacing is a resource now.
        if f == 5 {
            world.insert_resource(TimeScale(0.5));
        }
        let ms = sched.tick(&mut world);
        render
            .borrow_mut()
            .observe_frame(sched.avg_frame_ms(), sched.budget_ms());
        if f % 3 == 0 {
            println!("    frame {:2}: {:.2} ms", f, ms);
        }
    }
    world.insert_resource(TimeScale(1.0));

    // ── Report ──
    let pos_count = world.query::<Position>().count();
    let vel_count = world.query::<Velocity>().count();
    println!();
    println!("  ═══ Final Report ═══");
    println!("  ECS entities   : {}", world.len());
    println!("  Position comp  : {}", pos_count);
    println!("  Velocity comp  : {}", vel_count);
    println!("  Gravity        : {:?}", world.resource::<Gravity>());
    println!("  Stats (Count)  : {:?}", world.resource::<Stats>());
    println!(
        "  Avg frame      : {:.2} ms (budget {:.2} ms)",
        sched.avg_frame_ms(),
        sched.budget_ms()
    );
    println!("  Assets loaded  : {}", assets.len());
    println!(
        "  Render         : {:?} quality, {} draw calls",
        render.borrow().quality,
        render.borrow().draw_calls
    );
    println!();
    println!("  ── L2 per-system timings (last frame) ──");
    for (name, ms) in sched.system_timings() {
        println!("    {:<12} {:>8.3} ms", name, ms);
    }
    // v0.4 compute allocation: what the budget arbiter decided for the
    // frame — each system's share of the budget and its work-rate dial.
    // Everything reads 100% throttle while the frame fits its budget;
    // the dial only moves when the ledger is over-subscribed.
    println!();
    println!(
        "  ── L2 compute allocation (load {:.2}, headroom {:.0}%) ──",
        sched.allocator().load(sched.budget_ms()),
        sched.headroom() * 100.0
    );
    for (name, _) in sched.system_timings() {
        let g = sched.grants().get(name);
        println!(
            "    {:<12} share {:>7.2} ms   throttle {:>3.0}%",
            name,
            g.share_ms,
            g.throttle * 100.0
        );
    }
    // v0.7 (PLAN-deepseek-upgrade B5): the AI money readout, through the
    // same door any shell uses — UsageMeter counts tokens, CostMeter
    // values them. This offline demo makes no model calls, so both read
    // empty; the read path itself is the contract shells wire into.
    println!();
    println!("  ── AI meter (B5 cache/cost accounting) ──");
    match world.resource::<UsageMeter>() {
        Some(m) if m.calls > 0 => println!(
            "    tokens : {} prompt ({} cached) + {} completion in {} calls",
            m.total.prompt, m.total.cached, m.total.completion, m.calls
        ),
        _ => println!("    tokens : no model calls this run"),
    }
    match world.resource::<CostMeter>() {
        Some(c) if c.calls > 0 => println!(
            "    spend  : {} (cache saved {} µ{}, {} unpriced calls)",
            c.display(),
            c.saved_micro(),
            c.currency,
            c.unpriced_calls
        ),
        _ => println!("    spend  : no model calls this run"),
    }
    println!();
    println!("  ✅ Closed loop: L9 parse → L7 execute → L6 integrate → L5 count");
}

/// Runs the shared [`RenderSystem`] inside the scheduler while the main
/// loop keeps a handle to the same instance for the quality governor.
struct SharedRender(std::rc::Rc<std::cell::RefCell<RenderSystem>>);

impl oriera_core::System<World> for SharedRender {
    fn name(&self) -> &str {
        "render"
    }
    fn run(&mut self, ctx: &oriera_core::FrameCtx, world: &mut World) {
        self.0.borrow_mut().run(ctx, world);
    }
}
