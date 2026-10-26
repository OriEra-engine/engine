<div align="center">

<img src="assets/logo.png" width="200" alt="OriEra logo" />

# OriEra World Engine

**AI-native, mixed-reality, self-evolving open digital world runtime**

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![crates.io](https://img.shields.io/crates/v/oriera-agent.svg)](https://crates.io/crates/oriera-agent)
[![CI](https://github.com/OriEra-engine/engine/actions/workflows/ci.yml/badge.svg)](https://github.com/OriEra-engine/engine/actions/workflows/ci.yml)
[![Live Demo](https://img.shields.io/badge/demo-live-ff7a3d.svg)](https://oriera-engine.github.io/engine/)
[![Discussions](https://img.shields.io/badge/discussions-join-5b8def.svg)](https://github.com/OriEra-engine/engine/discussions)
[![AtomGit](https://atomgit.com/OriEra-engine/engine/star/badge.svg)](https://atomgit.com/OriEra-engine/engine)

[中文文档](README.md) · English

</div>

<p align="center">
  <img src="assets/hero.gif" alt="OriEra demo: AI NPC with long-term memory × million-particle world generation" width="640" />
</p>
<p align="center">
  <em>Top: Corner Café — 3D AI NPC chat + long-term memory + AI director ｜ Bottom: natural-language world generation</em><br/>
  <a href="docs/media/oriera-official-demo-en.mp4">📹 Full 54-second official demo (English subtitles)</a> · <a href="docs/media/oriera-official-demo.mp4">中文原版</a>
</p>

---

## What is this

OriEra is not another game engine. It is a real-time 3D world runtime that runs
on PC, mobile, web and AR glasses (BCI reserved for later) — a world that is
**driven by AI, keeps evolving itself, and can be created with natural language**.

Architecturally it is an **11-layer stack** with five first-class platforms.

## Try it (zero install)

**🌐 https://oriera-engine.github.io/engine/** — 13 playable demos in your browser.

New: [Memory Atlas](https://oriera-engine.github.io/engine/examples/12-memory-atlas/index.html) — record NPC memories, recall relevant episodes, and explore recency offline using the existing browser kernel. New: [Generative Gallery](https://oriera-engine.github.io/engine/examples/13-genai-gallery/index.html) — type a theme, the engine's L8 generates sky / ground / posters and hot-swaps them into a small 3D world (offline procedural fallback; bring a CogView/Tripo key for real generation).

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

- 🗺️ [Open source guide](docs/OPEN_SOURCE_GUIDE.en.md) — how to join, with or without writing code
- 🐣 [15-minute first PR guide](docs/FIRST_PR.md) (Chinese)
- 🎃 **Hacktoberfest 2026 is live** — all [good first issues](https://github.com/OriEra-engine/engine/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) count until Oct 31
- 📚 [Book ↔ repo map](docs/BOOK_MAP.md) — the architecture book is our spec
- 💬 [Discussions](https://github.com/OriEra-engine/engine/discussions) — questions in EN or 中文 both welcome
- 🗺️ [ROADMAP](ROADMAP.md) — L8/L10/L11 landing schedule

### Acknowledgments

- **[AtomGit](https://atomgit.com/OriEra-engine/engine)**: hosts an in-China mirror of this project, helping mainland China users access the project and download Releases faster.

## License

Apache License 2.0 — patent grant included, cloud-friendly.

---

<p align="center">
  <sub>If OriEra shows you another possible future for AGI-era infrastructure,<br/>
  please give this repo a ⭐ Star — it decides how far this open-source path can go.</sub><br/><br/>
  <a href="https://github.com/OriEra-engine/engine/stargazers"><img src="https://img.shields.io/badge/Star%20this%20repo-⭐-ffc861?style=for-the-badge" alt="Star OriEra" /></a>
</p>
