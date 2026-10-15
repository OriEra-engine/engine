//! L8 end-to-end demo: natural language → generated asset → hot swap
//! into the running store. Runs fully offline on the MockProvider —
//! no key, no network (`cargo run -p oriera-genai --example hotswap`).
//!
//! The L9 bridge (`oriera_agent::genai::parse_generation`) is not used
//! here so the example stays inside one layer; the cafe demo wires it
//! the same way `parse()` is wired for commands.

use oriera_asset::AssetStore;
use oriera_core::Scheduler;
use oriera_ecs::World;
use oriera_genai::{
    apply_pending, AssetLanded, GenAiMeter, GenAiSystem, GenAsset, GenerationRequest, MockProvider,
};
use std::sync::Arc;

/// Marker component: one entity per landed prop, spawned by the shell
/// off the L3 event queue — the world reacts to generation.
#[derive(Debug)]
struct Prop(String);

fn main() {
    println!("╔══════════════════════════════════════════════╗");
    println!("║  OriEra L8 · genai hot swap (mock, keyless)  ║");
    println!("╚══════════════════════════════════════════════╝");

    // ── The shell owns the world and the asset store (book T3) ──
    let mut world = World::new();
    let mut assets = AssetStore::new();

    // A pre-existing named asset the generation will REPLACE in place.
    assets.insert_named(
        "npc_prop",
        GenAsset {
            format: oriera_genai::AssetFormat::Glb,
            bytes: b"old-handmade-placeholder".to_vec(),
            provider_id: "hand-authored".into(),
            model: String::new(),
            placeholder: true,
        },
    );
    let before_bytes = assets
        .get_named::<GenAsset>("npc_prop")
        .map(|a| a.bytes.len())
        .unwrap_or(0);
    println!("  Before   : npc_prop = {before_bytes} bytes (hand-authored)");

    // ── L8 system on the L2 scheduler, offline provider ──
    let mut genai = GenAiSystem::new(Arc::new(MockProvider::new()));
    genai.submit(GenerationRequest::text_to_3d(
        "npc_prop",
        "一把做旧的木吉他，暖黄色漆面",
    ));
    genai.submit(GenerationRequest::text_to_3d(
        "bgm_intro",
        "雨夜咖啡馆的门帘被风吹起",
    ));

    let mut sched: Scheduler<World> = Scheduler::new();
    sched.add_system(Box::new(genai));

    println!("\n  Running 8 frames...");
    for f in 0..8 {
        let ms = sched.tick(&mut world);
        let landed = apply_pending(&mut assets, &mut world);
        for ev in world.take_events::<AssetLanded>() {
            let e = world.spawn();
            world.insert(e, Prop(ev.name.clone()));
            println!(
                "    frame {}: {:.2} ms — landed {:<10} by {}{}",
                f,
                ms,
                ev.name,
                ev.provider,
                if ev.placeholder { " (placeholder)" } else { "" }
            );
        }
        if landed > 0 {
            println!(
                "    frame {}: swapped {} asset(s) into the store",
                f, landed
            );
        }
    }

    // ── Report ──
    let meter = world.resource::<GenAiMeter>().copied().unwrap_or_default();
    let after = assets.get_named::<GenAsset>("npc_prop").unwrap();
    let bgm = assets.get_named::<GenAsset>("bgm_intro").unwrap();
    println!();
    println!("  ═══ Report ═══");
    println!(
        "  npc_prop : {} → {} bytes (provider {})",
        before_bytes,
        after.bytes.len(),
        after.provider_id
    );
    println!(
        "  bgm_intro: {} bytes, glTF magic = {}",
        bgm.bytes.len(),
        after.bytes.starts_with(b"glTF") && bgm.bytes.starts_with(b"glTF")
    );
    println!(
        "  Meter    : submitted {} · succeeded {} · failed {} · degraded {} · bytes_in {}",
        meter.submitted, meter.succeeded, meter.failed, meter.degraded, meter.bytes_in
    );
    let props: Vec<String> = world.query::<Prop>().map(|(_, p)| p.0.clone()).collect();
    println!(
        "  Props    : {} entities from AssetLanded events: [{}]",
        props.len(),
        props.join(", ")
    );
    println!("  Store    : {} asset(s)", assets.len());
    println!();
    println!("  ── L2 per-system timings (last frame) ──");
    for (name, ms) in sched.system_timings() {
        println!("    {:<12} {:>8.3} ms", name, ms);
    }
    println!();
    println!("  ✅ Closed loop: L8 request → poll → validate → PendingSwaps → L4 insert_named");
}
