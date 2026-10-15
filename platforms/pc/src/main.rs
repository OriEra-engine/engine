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
use oriera_subsys::{Gravity, PhysicsSystem};

fn main() {
    tracing_subscriber::fmt::init();

    // ── L1 PAL: detect platform ──
    let profile = oriera_pal::detect_platform();
    println!("╔══════════════════════════════════════╗");
    println!("║   OriEra World Engine v0.0.1 (PC)    ║");
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

    // ── L4 Asset: load a fake texture ──
    #[derive(Debug)]
    struct Texture {
        width: u32,
        height: u32,
    }
    let mut assets = AssetStore::new();
    let tex = assets.load(Texture {
        width: 512,
        height: 512,
    });
    let loaded = assets.get::<Texture>(tex).expect("just loaded");
    println!(
        "  Assets   : texture {}×{} ({:?})",
        loaded.width, loaded.height, tex
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

    // ── L2 Core: build the scheduler over the world ──
    let mut sched: Scheduler<World> = Scheduler::new();
    sched.add_system(Box::new(infer));
    sched.add_system(Box::new(PhysicsSystem::new(800.0, 600.0)));
    sched.add_system(Box::new(RenderSystem::new()));

    // ── Run the loop ──
    println!();
    println!("  Running 10 frames...");
    for f in 0..10 {
        let ms = sched.tick(&mut world);
        if f % 3 == 0 {
            println!("    frame {:2}: {:.2} ms", f, ms);
        }
    }

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
    println!("  Avg frame      : {:.2} ms", sched.avg_frame_ms());
    println!("  Assets loaded  : {}", assets.len());
    println!();
    println!("  ✅ Closed loop: L9 parse → L7 execute → L6 integrate → L5 count");
}
