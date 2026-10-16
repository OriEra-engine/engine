# Open Source Guide

> Written for first-time visitors to the OriEra community — whether or not you write code.
> For the 15-minute fast track to your first PR, see [FIRST_PR.md](FIRST_PR.md); this page is the full map.

中文 · **English**

## 1. The OriEra Community at a Glance

| Entry point | Address | Use it for |
|---|---|---|
| Code repository | https://github.com/OriEra-engine/engine | star / fork / open PRs |
| Live demos | https://oriera-engine.github.io/engine/ | 11 playable demos in your browser, zero install |
| Discussions | https://github.com/OriEra-engine/engine/discussions | questions, chat, RFCs (English or 中文) |
| Official demo video | `docs/media/oriera-official-demo.mp4` (in-repo, 54s) | both AI-native paradigms in one minute |
| Architecture doc | [ARCHITECTURE.md](ARCHITECTURE.md) (Chinese; EN translation welcome) | 11 layers, dependency rules, per-frame dataflow |
| Biweekly newsletter | [Discussions · Announcements](https://github.com/OriEra-engine/engine/discussions/categories/announcements) | what shipped every two weeks |

**License: Apache 2.0** (patent grant included, commercial use friendly).

## 2. Five-Minute Quick Start

1. **Star** the repo — the cheapest support you can give, and how others find us.
2. Open the [live demos](https://oriera-engine.github.io/engine/) and play
   [10 Word Spirit](https://oriera-engine.github.io/engine/examples/10-word-spirit/index.html)
   (rewrite physics laws by speaking) and
   [11 Corner Café](https://oriera-engine.github.io/engine/examples/11-ai-cafe/index.html)
   (3D AI NPC + long-term memory + AI director).
3. Run locally: `git clone` then `npx serve .` (web demos, no Rust needed),
   or `cargo run -p oriera-pc` (Rust engine, 1.80+).

## 3. The Participation Ladder: Onlooker → Core Team

Every step is voluntary, and stopping at any rung is welcome:

```
Onlooker (star / play demos)
   └─ User (bug reports, feedback)
        └─ Contributor (first PR merged — you're in)
             └─ Active contributor (reviews, RFCs)
                  └─ Core Team (merge access, module ownership)
```

Formal roles and decision flows live in [GOVERNANCE.md](../GOVERNANCE.md):
early-stage BDFL + Core Team; small changes merge with 1 approval,
cross-layer or contract changes go through a 7-day RFC comment period.

## 4. Contributing Without Writing Code

| Type | How |
|---|---|
| Demo feedback | Open an issue: which demo + what you did + expected vs actual |
| Doc translation | EN ↔ 中文 for the READMEs and architecture docs; PR directly |
| Tutorials / case studies | "I built X with OriEra" → Discussions · Show and tell |
| Gameplay ideas | New "natural language rewrites the rules" paradigms à la Word Spirit — open an issue |
| Bug hunting | Stress a demo (rapid clicks / huge values / offline); if reproducible, open an issue |

## 5. Writing Code: Start With a good first issue

1. Open the [good first issue list](https://github.com/OriEra-engine/engine/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) —
   every issue lists the **files involved and acceptance criteria**.
2. Comment to claim it ("I'll take this") to avoid collisions.
3. Follow the 15-minute flow in [FIRST_PR.md](FIRST_PR.md): fork → branch → change →
   `cargo fmt` + `clippy` + `test` → commit with DCO sign-off → PR.

Two hard requirements (details in [CONTRIBUTING.md](../CONTRIBUTING.md)):

- **Conventional Commits**: `feat(pal): add android spatial backend`
- **DCO sign-off**: `git commit -s` adds `Signed-off-by:` (you attest you have the
  right to contribute — no CLA needed)

## 6. Community Norms

- **Code of Conduct**: [CODE_OF_CONDUCT.md](../CODE_OF_CONDUCT.md) (Contributor Covenant 2.1).
  One-liner: critique code, not people — beginner questions are welcome here.
- **How to ask**: search existing Discussions first; include environment
  (browser/Rust version) + the full error + repro steps.
- **Language**: issue / PR / Discussions in English or 中文 both work.
  PR titles in English are appreciated (easier to search); bodies in Chinese are fine.
- **Security vulnerabilities**: never open a public issue — see [SECURITY.md](../SECURITY.md).
- **Absolute red line**: never commit API keys / model tokens / user private data.

## 7. Hacktoberfest (every October)

OriEra participates in Hacktoberfest: merge **4** qualifying PRs during October
to earn the official digital reward. The repo keeps a batch of good first issues
labeled `hacktoberfest`, ranging from doc typos to demo features.
Note that typo-only PRs may not count — picking a labeled issue is the safe route.

## 8. FAQ

**I don't know Rust. Can I still contribute?**
Yes. The demos under `examples/` are plain HTML/JS, and docs, translations and
test feedback need no Rust at all.

**I'm a student. Can I participate?**
Yes — open source looks at PRs, not diplomas. good first issues spell out
their acceptance criteria; just follow them.

**Can my company use it?**
Apache 2.0 with patent grant, commercial-friendly. Just keep the license
notices and copyright headers intact.

**How do I reach the maintainers?**
Post in any Discussions category, or join the Chinese community chat
(WeChat/Feishu — ask any group admin for an invite).

**Can I change the architecture / add a layer?**
Yes, but open an RFC in
[Discussions · Ideas](https://github.com/OriEra-engine/engine/discussions/categories/ideas)
first with a 7-day comment period — that's how we protect the eleven-layer
boundaries from accidental damage.
