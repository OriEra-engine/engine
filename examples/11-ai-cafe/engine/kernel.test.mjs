// kernel v2 测试（node --test 直测浏览器内核；kernel 已对无 localStorage
// 环境静默降级）。运行：node --test examples/11-ai-cafe/engine/kernel.test.mjs
import test from "node:test";
import assert from "node:assert/strict";
import {
  MemoryStore, HttpChatProvider, parseIntent, CommandQueue,
  CognitionSystem, DirectorSystem, PROVIDER_PRESETS, createProvider,
} from "./kernel.js";

/* ---- parseIntent：与 Rust L9 parse 同词汇表（镜像断言） ---- */
test("parse 镜像：spawn/clear/gravity/teleport/count/speed", () => {
  assert.deepEqual(parseIntent("spawn 40 particles"), { kind: "spawn", n: 40 });
  assert.deepEqual(parseIntent("放 20 个"), { kind: "spawn", n: 20 });
  assert.deepEqual(parseIntent("生成3个"), { kind: "spawn", n: 3 });
  assert.deepEqual(parseIntent("clear"), { kind: "clear" });
  assert.deepEqual(parseIntent("清空"), { kind: "clear" });
  assert.deepEqual(parseIntent("set gravity to 1.6"), { kind: "gravity", g: 1.6 });
  assert.deepEqual(parseIntent("重力 9.8"), { kind: "gravity", g: 9.8 });
  assert.deepEqual(parseIntent("teleport 100 -25"), { kind: "teleport", dx: 100, dy: -25 });
  assert.deepEqual(parseIntent("how many"), { kind: "count" });
  assert.deepEqual(parseIntent("有多少个"), { kind: "count" });
  assert.deepEqual(parseIntent("speed 2x"), { kind: "speed", k: 2 });
  assert.deepEqual(parseIntent("倍速 0.5"), { kind: "speed", k: 0.5 });
});

test("否定语义先于意图 → unknown（别清空不清空）", () => {
  assert.equal(parseIntent("别清空").kind, "unknown");
  assert.equal(parseIntent("不要生成新的").kind, "unknown");
  assert.equal(parseIntent("do not spawn").kind, "unknown");
  // spawn 无数字 → 落空为 unknown（与 Rust 一致：意图词在但参数缺失不猜测）
  assert.equal(parseIntent("spawn it, 别 cancel").kind, "unknown");
});

test("无法识别 → unknown 且携带原文", () => {
  const c = parseIntent("给我讲个故事");
  assert.equal(c.kind, "unknown");
  assert.equal(c.text, "给我讲个故事");
});

/* ---- CommandQueue：预算/合并/拒绝计数 ---- */
test("队列：相邻 spawn 合并、count 重复合一、unknown 计拒绝", () => {
  const seen = [];
  const q = new CommandQueue((cmd) => seen.push(cmd), { frameBudget: 8 });
  q.submit("spawn 40");
  q.submit("spawn 20");   // 合并 → 60
  q.submit("多少");        // count
  q.submit("有多少个");    // 与上一条合并
  q.submit("别清空");      // unknown → rejected
  q.submit("clear");
  assert.equal(q.coalesced, 2);
  q.tick({});
  assert.equal(q.processed, 3); // spawn(60) + count + clear
  assert.equal(q.rejected, 1);  // unknown 在执行期计数
  assert.deepEqual(seen, [{ kind: "spawn", n: 60 }, { kind: "count" }, { kind: "clear" }]);
});

test("帧预算：一次 tick 只消化预算条数，不饿死", () => {
  const q = new CommandQueue(() => {}, { frameBudget: 2 });
  q.submit("clear"); q.submit("多少"); q.submit("clear"); q.submit("多少");
  q.tick({});
  assert.equal(q.pending, 2);
  q.tick({});
  assert.equal(q.pending, 0);
});

/* ---- MemoryStore：容量参数化 + 无 localStorage 可用 ---- */
test("记忆容量可注入且默认 60", () => {
  const m60 = new MemoryStore("test.mem60");
  for (let i = 0; i < 70; i++) m60.record("a", { turn: i, day: 1, about: "w", text: "t" + i, imp: 1 });
  assert.equal(m60.lenTotal(), 60);
  const m5 = new MemoryStore("test.mem5", 5);
  for (let i = 0; i < 9; i++) m5.record("a", { turn: i, day: 1, about: "w", text: "t" + i, imp: 1 });
  assert.equal(m5.lenTotal(), 5);
  assert.equal(m5.retrieve("a", "w", "t8", 100, 1)[0].text, "t8"); // 最新保留
});

/* ---- HttpChatProvider：seed/thinking 透传（stub fetch 抓请求体） ---- */
test("provider 透传 seed 与 thinking（仅 openai 兼容方言）", async () => {
  let captured = null;
  const origFetch = globalThis.fetch;
  globalThis.fetch = async (_u, o) => {
    captured = JSON.parse(o.body);
    return { ok: true, json: async () => ({ choices: [{ message: { content: "ok" } }], usage: { total_tokens: 5 } }) };
  };
  try {
    const p = new HttpChatProvider({ kind: "deepseek", key: "test", model: "deepseek-flash", base: "https://x" });
    await p.complete([{ role: "user", content: "hi" }], { seed: 42, thinking: { type: "disabled" } });
    assert.equal(captured.seed, 42);
    assert.deepEqual(captured.thinking, { type: "disabled" });
    assert.equal(p.stats.errors, 0);
  } finally { globalThis.fetch = origFetch; }
});

/* ---- CognitionSystem：场景/台词库/RNG/max 注入 ---- */
test("认知系统：注入场景与 max 后 prompt 走新世界，默认仍是咖啡馆", async () => {
  let capturedSys = null, capturedOpts = null;
  const origFetch = globalThis.fetch;
  globalThis.fetch = async (_u, o) => {
    const body = JSON.parse(o.body);
    capturedSys = body.messages[0].content;
    capturedOpts = body;
    return { ok: true, json: async () => ({ choices: [{ message: { content: '{"say":"收到","action":"idle","mood":"neutral","remember":"","to":"客人"}' } }] }) };
  };
  try {
    const world = { turn: 0, npcs: [{ id: "zhou", name: "老周", role: "老板", description: "手冲派", mood: "neutral" }], dialogue: [], story: { day: 1, beats: [] }, guestQueue: [] };
    const mem = new MemoryStore("test.cog");
    const space = new CognitionSystem(
      new HttpChatProvider({ kind: "deepseek", key: "k", model: "m", base: "https://x" }),
      mem, { scene: "环形太空站", maxTokens: 120 },
    );
    await space.tick(world);
    assert.ok(capturedSys.includes("环形太空站"), "注入场景生效");
    assert.ok(!capturedSys.includes("隅角咖啡馆"), "旧场景不残留");
    assert.equal(capturedOpts.max_tokens, 120);
    assert.ok(capturedSys.includes("隅角咖啡馆") === false);

    const cafe = new CognitionSystem(
      new HttpChatProvider({ kind: "deepseek", key: "k", model: "m", base: "https://x" }),
      new MemoryStore("test.cog2"), {},
    );
    capturedSys = null;
    await cafe.tick({ ...world, turn: 0 });
    assert.ok(capturedSys.includes("隅角咖啡馆"), "默认场景保持 v1 兼容");
    assert.equal(capturedOpts.max_tokens, 260);
  } finally { globalThis.fetch = origFetch; }
});

test("认知系统：注入 rng 后离线台词可复现", () => {
  const world = { turn: 0, npcs: [{ id: "zhou", name: "老周", role: "老板", description: "手冲派", mood: "neutral" }], dialogue: [], story: { day: 1, beats: [] }, guestQueue: [] };
  let seed = 1234;
  const rng = () => (seed = (seed * 1103515245 + 12345) % 2147483648) / 2147483648;
  const run = () => {
    seed = 1234;
    const c = new CognitionSystem({ ready: false }, new MemoryStore("test.rng"), { rng });
    return c._rule({ id: "zhou", name: "老周" }, { name: "客人" }, world).say;
  };
  assert.equal(run(), run());
});

test("导演系统：注入 ruleBeats 生效", async () => {
  const d = new DirectorSystem({ ready: false }, new MemoryStore("test.dir"), { ruleBeats: ["甲事件", "乙事件"] });
  const world = { turn: 0, dialogue: [], story: { day: 1, beats: [] } };
  world.turn = 8; // interval 8 → tick 生效
  const beat = await d.tick(world);
  assert.equal(beat, "甲事件");
});

/* ---- 预设表：13 行与 Rust PROVIDER_PRESETS 对齐 ---- */
test("预设表 13 行（rule 领头 + 云端 8 + 本地开源 4）", () => {
  const ids = Object.keys(PROVIDER_PRESETS);
  assert.equal(ids.length, 13);
  assert.equal(ids[0], "rule");
  for (const id of ["qwen", "ollama", "vllm", "llamacpp", "lmstudio"]) assert.ok(ids.includes(id), id);
  assert.equal(PROVIDER_PRESETS.ollama.base, "http://127.0.0.1:11434/v1");
  assert.equal(PROVIDER_PRESETS.qwen.base, "https://dashscope.aliyuncs.com/compatible-mode/v1");
});

test("createProvider 兼容：rule/无 key 返回 null 走降级", () => {
  assert.equal(createProvider({ kind: "rule" }), null);
  assert.equal(createProvider({ kind: "deepseek" }), null);
  assert.ok(createProvider({ kind: "deepseek", key: "k", model: "m", base: "https://x" }));
});
