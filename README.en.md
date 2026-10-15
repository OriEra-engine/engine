<div align="center">

# OriEra World Engine

**AI-native, mixed-reality, self-evolving open digital world runtime**

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![CI](https://github.com/OriEra-engine/engine/actions/workflows/ci.yml/badge.svg)](https://github.com/OriEra-engine/engine/actions/workflows/ci.yml)
[![Live Demo](https://img.shields.io/badge/demo-live-ff7a3d.svg)](https://oriera-engine.github.io/engine/)
[![Discussions](https://img.shields.io/badge/discussions-join-5b8def.svg)](https://github.com/OriEra-engine/engine/discussions)

[中文文档](README.md) · English

</div>

---

## What is this

OriEra is not another game engine. It is a real-time 3D world runtime that runs
on PC, mobile, web and AR glasses (BCI reserved for later) — a world that is
**driven by AI, keeps evolving itself, and can be created with natural language**.

Architecturally it is an **11-layer stack** with five first-class platforms.

## Try it (zero install)

**🌐 https://oriera-engine.github.io/engine/** — 9 playable demos in your browser.

## Architecture

Eleven layers, each a responsibility boundary connected by explicit contracts.
Layer ↔ crate mapping, dependency rules and the per-frame data flow live in
**[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** (Chinese; English translation welcome — see
[good first issues](https://github.com/OriEra-engine/engine/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22)).

Every layer ships a machine-readable manifest (`manifests/`) declaring what it
provides, requires, its budgets and degradation strategy.

## Quick start

```bash
git clone https://github.com/OriEra-engine/engine.git
cd engine
npx serve .           # open http://localhost:3000 for the web demos

# Rust side (1.80+)
cargo test --workspace
cargo run -p oriera-pc
```

## Community

- 🐣 [15-minute first PR guide](docs/FIRST_PR.md) (Chinese)
- 📚 [Book ↔ repo map](docs/BOOK_MAP.md) — the architecture book is our spec
- 💬 [Discussions](https://github.com/OriEra-engine/engine/discussions) — questions in EN or 中文 both welcome
- 🗺️ [ROADMAP](ROADMAP.md) — L8/L10/L11 landing schedule

## License

Apache License 2.0 — patent grant included, cloud-friendly.
