// Demo 14 — film-game kernel (L10 browser mirror of oriera-agent/story.rs)
// 逐句对照 crates/oriera-agent/src/story.rs：同一 schema、同一结算语义、
// 同一 FNV 指纹。index.html 是应用壳，本文件是引擎镜像——延续 demo 11/13
// 的「Rust↔JS 一一对照」传统。Golden 指纹与 Rust 测试共享（见下方断言）。
//
// 用法（零依赖，双击 index.html 即自检）：
//   const g = FilmStory.parse(json); FilmStory.validate(g); ...

const FilmStory = (() => {
  // ── schema ──
  function parse(json) {
    const d = typeof json === "string" ? JSON.parse(json) : json;
    return {
      vars: new Map(Object.entries(d.vars ?? {})),
      nodes: (d.nodes ?? []).map((n) => ({
        id: n.id,
        kind: n.type,
        title: n.title ?? "",
        goto: n.goto ?? null,
        options: (n.options ?? []).map((o) => ({
          text: o.text,
          set: new Map(Object.entries(o.set ?? {})),
          goto: o.goto,
        })),
        routes: (n.routes ?? []).map((r) => ({ when: r.when ?? null, goto: r.goto })),
        timeout_secs: n.timeout ?? null,
      })),
    };
  }

  // ── 结算语义（与 Rust apply_one 逐分支对齐） ──
  function asInt(v) {
    if (typeof v === "number") return v;
    if (typeof v === "boolean") return v ? 1 : 0;
    return 0;
  }

  function applyOne(cur, expr) {
    const e = String(expr).trim();
    if (e.startsWith("+") || e.startsWith("-")) {
      const d = parseInt(e, 10);
      if (!Number.isNaN(d)) return asInt(cur) + d;
    }
    if (e === "true") return true;
    if (e === "false") return false;
    const n = parseInt(e, 10);
    return Number.isNaN(n) ? e : n;
  }

  function applySets(graph, vars, sets) {
    for (const [k, expr] of sets) {
      if (vars.has(k)) vars.set(k, applyOne(vars.get(k), expr));
    }
  }

  function splitWhen(expr) {
    for (const op of ["<=", ">=", "==", "!=", "<", ">"]) {
      const i = expr.indexOf(op);
      if (i > 0) {
        const name = expr.slice(0, i).trim();
        if (/^[A-Za-z0-9_]+$/.test(name)) return [name, op, expr.slice(i + op.length).trim()];
      }
    }
    return null;
  }

  function evalWhen(vars, expr) {
    const p = splitWhen(String(expr).trim());
    if (!p) return null;
    const [name, op, rhs] = p;
    if (!vars.has(name)) return null;
    const b = parseInt(rhs, 10);
    if (Number.isNaN(b)) return null;
    const a = asInt(vars.get(name));
    return { ">=": a >= b, "<=": a <= b, ">": a > b, "<": a < b, "==": a === b, "!=": a !== b }[op];
  }

  // resolve_hub：first-true-else-default（与 Rust/播放器三方同约）
  function resolveHub(vars, node) {
    let fallback = null;
    for (const r of node.routes) {
      if (!r.when && !fallback) fallback = r;
      if (r.when && evalWhen(vars, r.when) === true) return r;
    }
    return fallback ?? node.routes[node.routes.length - 1] ?? null;
  }

  // ── 校验器（与 Rust validate 同一发现集） ──
  function validate(g) {
    const out = [];
    if (!g.nodes.length) return [{ severity: "error", node: "-", kind: "empty" }];
    const ids = new Set(g.nodes.map((n) => n.id));
    const declared = new Set([...g.vars.keys()]);
    const outs = (n) => [
      ...n.options.map((o) => o.goto),
      ...n.routes.map((r) => r.goto),
      ...(n.goto ? [n.goto] : []),
    ];
    for (const n of g.nodes) {
      if (n.kind === "scene" && !n.goto) out.push(err(n, "dead_end", "scene without goto"));
      if (n.kind === "choice" && !n.options.length)
        out.push(err(n, "choice_without_options", "choice without options"));
      if (n.kind === "hub" && !n.routes.length) out.push(err(n, "dead_end", "hub without routes"));
      if (n.kind === "hub" && n.routes.length && n.routes.every((r) => r.when))
        out.push({ severity: "warning", node: n.id, kind: "hub_without_default" });
      for (const t of outs(n))
        if (!ids.has(t)) out.push(err(n, "missing_goto", `goto target does not exist: ${t}`));
      for (const o of n.options)
        for (const v of o.set.keys())
          if (!declared.has(v)) out.push(err(n, "unknown_variable", `undeclared: ${v}`));
      for (const r of n.routes) {
        if (!r.when) continue;
        const p = splitWhen(r.when);
        if (!p) out.push({ severity: "warning", node: n.id, kind: "unknown_variable", detail: r.when });
        else if (!declared.has(p[0])) out.push(err(n, "unknown_variable", `undeclared: ${p[0]}`));
      }
    }
    // 可达性（BFS，与 Rust 同）
    const byId = new Map(g.nodes.map((n) => [n.id, n]));
    const seen = new Set();
    const q = [g.nodes[0].id];
    while (q.length) {
      const id = q.shift();
      if (seen.has(id)) continue;
      seen.add(id);
      for (const t of outs(byId.get(id))) if (byId.has(t) && !seen.has(t)) q.push(t);
    }
    for (const n of g.nodes) if (!seen.has(n.id)) out.push(err(n, "unreachable", "unreachable from entry"));
    if (![...g.nodes].some((n) => n.kind === "ending" && seen.has(n.id)))
      out.push({ severity: "error", node: "-", kind: "unreachable", detail: "no reachable ending" });
    return out;
  }

  function err(n, kind, detail) {
    return { severity: "error", node: n.id, kind, detail };
  }

  // ── 存档指纹（与 Rust save_fingerprint 同 FNV-1a/同种子/同序） ──
  function saveFingerprint(vars, nodeId) {
    // 对齐 Rust: 0xcbf29ce484222325 ^ u64::from_le_bytes(*b"oriera.s")
    let h = 0xcbf29ce484222325n ^ 0x732e61726569726fn;
    const feed = (bytes) => {
      for (const b of bytes) {
        h ^= BigInt(b);
        h = (h * 0x100000001b3n) & 0xffffffffffffffffn;
      }
    };
    for (const [k, v] of [...vars.entries()].sort((a, b) => (a[0] < b[0] ? -1 : 1))) {
      feed(new TextEncoder().encode(k));
      feed([0x1f]);
      if (typeof v === "number") feed(new Uint8Array(new BigInt64Array([BigInt(v)]).buffer));
      else if (typeof v === "boolean") feed([v ? 1 : 0]);
      else feed(new TextEncoder().encode(v));
      feed([0x1e]);
    }
    feed(new TextEncoder().encode(nodeId));
    return h;
  }

  // ── 氛围导演（与 Rust atmosphere 同映射） ──
  function atmosphere(vars) {
    const trust = asInt(vars.get("trust") ?? 0);
    const affinity = asInt(vars.get("affinity_lin") ?? 0);
    if (trust <= -2 || affinity <= -2) return "tension_peak";
    if (trust < 0) return "rain_heavier";
    if (trust >= 2) return "sirens_on";
    return "calm";
  }

  // ── choice 记忆（与 Rust choice_memories 同形） ──
  function choiceMemories(g) {
    return g.nodes
      .filter((n) => n.kind === "choice")
      .flatMap((n) =>
        n.options.map((o) => ({
          node_id: n.id,
          option_text: o.text,
          touched_vars: [...o.set.keys()],
        })),
      );
  }

  return { parse, applySets, evalWhen, resolveHub, validate, saveFingerprint, atmosphere, choiceMemories };
})();

// 双宿主（demo 11/13 惯例）：Node 走 require，浏览器挂 window。
if (typeof module !== "undefined") {
  module.exports = { FilmStory };
}
if (typeof window !== "undefined") {
  window.FilmStory = FilmStory;
}
