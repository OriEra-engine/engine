//! L5 Render — render graph and frame submission.
//!
//! v0.1 counts visible `Position` components as a stand-in for a real
//! render graph (book T8). The world comes from the L2 scheduler; render
//! depends only on layers below it (L2 core, L3 object model) — never on
//! L6 subsystems.

use oriera_core::{FrameCtx, System};
use oriera_ecs::{Position, World};

/// Frame submission system: cull + count what would be drawn.
pub struct RenderSystem {
    pub draw_calls: u32,
    pub visible: u32,
}

impl RenderSystem {
    pub fn new() -> Self {
        Self {
            draw_calls: 0,
            visible: 0,
        }
    }
}

impl System<World> for RenderSystem {
    fn name(&self) -> &str {
        "render"
    }
    fn run(&mut self, _ctx: &FrameCtx, world: &mut World) {
        self.visible = world.query::<Position>().count() as u32;
        self.draw_calls = self.visible.max(1);
    }
}

impl Default for RenderSystem {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oriera_common::FrameIndex;

    #[test]
    fn counts_visible_positions() {
        let mut world = World::new();
        for i in 0..5 {
            let e = world.spawn();
            world.insert(
                e,
                Position {
                    x: i as f32,
                    y: 0.0,
                },
            );
        }
        let ctx = FrameCtx {
            frame: FrameIndex(0),
            dt: 0.0,
            now: oriera_common::Timestamp(0),
        };
        let mut sys = RenderSystem::new();
        sys.run(&ctx, &mut world);
        assert_eq!(sys.visible, 5);
        assert_eq!(sys.draw_calls, 5);
    }
}
