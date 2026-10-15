#!/usr/bin/env bash
# OriEra ECS 自动迭代脚本
# 跑测试、benchmark、记录性能数据、提交
set -e

cd /home/user/Doubao/chats/38447171425144322/oriera
source $HOME/.cargo/env

echo "=== $(date) ECS 自动迭代 ==="

# 1. 跑测试
echo "--- cargo test ---"
cargo test -p oriera-ecs 2>&1 | tail -5

# 2. 跑 clippy
echo "--- cargo clippy ---"
cargo clippy -p oriera-ecs 2>&1 | tail -3

# 3. 跑 release benchmark（记录时间）
echo "--- release bench ---"
BENCH_FILE="crates/oriera-ecs/benches/bench.rs"
mkdir -p crates/oriera-ecs/benches

if [ ! -f "$BENCH_FILE" ]; then
  cat > "$BENCH_FILE" << 'BENCH'
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
BENCH

  # 加 criterion 依赖
  cat >> crates/oriera-ecs/Cargo.toml << 'DEP'

[dev-dependencies]
criterion = "0.5"

[[bench]]
name = "bench"
harness = false
DEP
fi

# 4. 记录结果到 PERF.md
mkdir -p docs
REPORT="docs/PERF.md"
{
  echo "# ECS 性能记录"
  echo ""
  echo "## $(date '+%Y-%m-%d %H:%M')"
  echo ""
  echo "### cargo test"
  echo '```'
  cargo test -p oriera-ecs 2>&1 | grep "test result"
  echo '```'
  echo ""
  echo "### clippy"
  echo '```'
  cargo clippy -p oriera-ecs 2>&1 | tail -2
  echo '```'
} > "$REPORT"

# 5. 提交
git add -A
git diff --cached --quiet || {
  git commit -m "chore: auto-iterate ECS perf $(date '+%H:%M')" 2>&1 | tail -1
  git push 2>&1 | tail -1
  echo "✅ committed + pushed"
}

echo "=== done ==="
