// PC entry point — full engine boot loop.
// L1 PAL → L2 Core scheduler → L3 ECS → L4 Asset → L5 Render → L6 Physics → L7 Infer

use oriera_core::Scheduler;
use oriera_ecs::{World, Position, Velocity, Health};
use oriera_subsys::{PhysicsSystem, WORLD, set_world};
use oriera_render::RenderSystem;
use oriera_infer::{InferSystem, parse};
use oriera_asset::AssetStore;

fn main() {
    tracing_subscriber::fmt::init();

    // ── L1 PAL: detect platform ──
    let profile = oriera_pal::detect_platform();
    println!("╔══════════════════════════════════════╗");
    println!("║  OriEra World Engine v0.0.1         ║");
    println!("╚══════════════════════════════════════╝");
    println!("  Platform : {:?} / {}", profile.family, profile.os_name);
    println!("  Budget   : {} µs/frame", profile.frame_budget_us);
    println!("  Caps     : {:?}", profile.caps);
    println!();

    // ── L3 ECS: spawn initial entities ──
    let mut world = World::new();
    for i in 0..100 {
        let e = world.spawn();
        world.insert(e, Position {
            x: (i as f32 * 10.0) % 800.0,
            y: (i as f32 * 7.0) % 600.0,
        });
        world.insert(e, Velocity {
            vx: (i as f32 - 50.0) * 0.1,
            vy: (i as f32 - 50.0) * 0.05,
        });
        if i % 3 == 0 {
            world.insert(e, Health(100.0));
        }
    }
    set_world(world);
    println!("  ECS      : 100 entities spawned");

    // ── L4 Asset: load a fake texture ──
    #[derive(Debug)]
    struct Texture { #[allow(dead_code)] width: u32, #[allow(dead_code)] height: u32 }
    let mut assets = AssetStore::new();
    assets.load(Texture { width: 512, height: 512 });
    println!("  Assets   : {} loaded", assets.len());

    // ── L2 Core: build scheduler ──
    let mut sched = Scheduler::new();
    sched.add_system(Box::new(PhysicsSystem::new(800.0, 600.0)));
    sched.add_system(Box::new(RenderSystem::new()));
    let mut infer = InferSystem::new();

    // ── L7 Infer: simulate a chat command ──
    let cmd = parse("spawn 50 more particles");
    println!("  Infer    : parsed → {:?}", cmd);
    infer.submit(cmd);
    sched.add_system(Box::new(infer));

    // ── Run 10 frames ──
    println!();
    println!("  Running 10 frames...");
    for f in 0..10 {
        let ms = sched.tick();
        if f % 3 == 0 {
            println!("    frame {:2}: {:.2} ms", f, ms);
        }
    }

    // ── Report ──
    let (pos_count, vel_count, hp_count, live) = WORLD.with(|w| {
        let cell = w.borrow();
        let world = cell.as_ref().unwrap();
        (
            world.query::<Position>().count(),
            world.query::<Velocity>().count(),
            world.query::<Health>().count(),
            world.len(),
        )
    });

    println!();
    println!("  ═══ Final Report ═══");
    println!("  ECS entities   : {}", live);
    println!("  Position comp  : {}", pos_count);
    println!("  Velocity comp  : {}", vel_count);
    println!("  Health comp    : {}", hp_count);
    println!("  Avg frame      : {:.2} ms", sched.avg_frame_ms());
    println!("  Assets loaded  : {}", assets.len());
    println!();
    println!("  ✅ Full architecture boot: PAL → Core → ECS → Asset → Render → Physics → Infer");
}
