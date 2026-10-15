//! L5 Render — render graph and frame submission.

use oriera_core::{FrameCtx, System};
use oriera_ecs::Position;
use oriera_subsys::WORLD;

pub struct RenderSystem {
    pub draw_calls: u32,
    pub visible: u32,
}

impl RenderSystem {
    pub fn new() -> Self {
        Self { draw_calls: 0, visible: 0 }
    }
}

impl System for RenderSystem {
    fn name(&self) -> &str { "render" }
    fn run(&mut self, _ctx: &FrameCtx) {
        WORLD.with(|w| {
            let cell = w.borrow();
            if let Some(world) = cell.as_ref() {
                self.visible = world.query::<Position>().count() as u32;
                self.draw_calls = self.visible.max(1);
            }
        });
    }
}

impl Default for RenderSystem {
    fn default() -> Self { Self::new() }
}
