use criterion::{black_box, criterion_group, criterion_main, Criterion};
use oriera_ecs::{World, Position, Velocity};

fn bench_spawn_10k(c: &mut Criterion) {
    c.bench_function("spawn_10k", |b| {
        b.iter(|| {
            let mut world = World::new();
            for i in 0..10_000 {
                let e = world.spawn();
                world.insert(e, Position { x: i as f32, y: 0.0 });
                world.insert(e, Velocity { vx: 0.01, vy: 0.0 });
            }
            black_box(&world);
        })
    });
}

fn bench_query_10k(c: &mut Criterion) {
    let mut world = World::new();
    for i in 0..10_000 {
        let e = world.spawn();
        world.insert(e, Position { x: i as f32, y: 0.0 });
        world.insert(e, Velocity { vx: 0.01, vy: 0.0 });
    }
    c.bench_function("query_10k", |b| {
        b.iter(|| black_box(world.query2::<Position, Velocity>().count()));
    });
}

criterion_group!(benches, bench_spawn_10k, bench_query_10k);
criterion_main!(benches);
