// PC entry point — Win/macOS/Linux native.
// Future: open a wgpu window, run the frame loop.
fn main() {
    tracing_subscriber::fmt::init();
    let profile = oriera_pal::detect_platform();
    tracing::info!(?profile, "OriEra PC booted");
    println!("OriEra v0.0.1 — PC target");
    println!("  family   = {:?}", profile.family);
    println!("  os       = {}", profile.os_name);
    println!("  frame_us = {}", profile.frame_budget_us);
    println!("  caps     = {:?}", profile.caps);
}
