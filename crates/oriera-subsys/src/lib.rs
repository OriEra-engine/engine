//! L6 Subsystems — physics, animation, audio, UI, net sync.

use oriera_core::{FrameCtx, System};
use oriera_ecs::World;
use std::cell::RefCell;

thread_local! {
    pub static WORLD: RefCell<Option<World>> = const { RefCell::new(None) };
}

pub fn set_world(w: World) {
    WORLD.with(|cell| *cell.borrow_mut() = Some(w));
}

pub struct PhysicsSystem {
    pub bounds: (f32, f32),
    pub gravity: f32,
}

impl PhysicsSystem {
    pub fn new(w: f32, h: f32) -> Self {
        Self { bounds: (w, h), gravity: 0.0 }
    }
}

impl System for PhysicsSystem {
    fn name(&self) -> &str { "physics" }
    fn run(&mut self, ctx: &FrameCtx) {
        WORLD.with(|w| {
            let mut cell = w.borrow_mut();
            if let Some(world) = cell.as_mut() {
                world.integrate_physics(ctx.dt, self.gravity, self.bounds);
            }
        });
    }
}
