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
use oriera_infer::{InferSystem, Stats};
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
