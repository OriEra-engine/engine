import test from "node:test";
import assert from "node:assert/strict";
import { probeBrowser } from "./probe.js";

test("missing browser APIs have an empty baseline", async () => {
  assert.deepEqual(await probeBrowser({}), {
    webgpu: false, touch: false, keyboardMouse: false, webtransport: false,
  });
});
test("phone browser probes touch without assuming desktop input", async () => {
  const p = await probeBrowser({navigator: {maxTouchPoints: 5}});
  assert.equal(p.touch, true);
  assert.equal(p.keyboardMouse, false);
});
test("secure context and a real adapter enable WebGPU", async () => {
  const p = await probeBrowser({isSecureContext: true,
    navigator: {gpu: {requestAdapter: async () => ({})}},
    matchMedia: () => ({matches: true}), WebTransport: class {},
  });
  assert.equal(p.webgpu, true);
  assert.equal(p.keyboardMouse, true);
  assert.equal(p.webtransport, true);
});
test("null and rejected adapters disable WebGPU", async () => {
  for (const requestAdapter of [async () => null, async () => {throw Error("denied");}]) {
    assert.equal((await probeBrowser({isSecureContext: true,
      navigator: {gpu: {requestAdapter}}})).webgpu, false);
  }
});
test("insecure origins never request an adapter or advertise transport", async () => {
  const p = await probeBrowser({isSecureContext: false, WebTransport: class {},
    navigator: {gpu: {requestAdapter() {assert.fail("must not probe");}}}});
  assert.equal(p.webgpu, false);
  assert.equal(p.webtransport, false);
});
test("a stalled adapter cannot block startup indefinitely", async () => {
  const p = await probeBrowser({isSecureContext: true,
    navigator: {gpu: {requestAdapter: () => new Promise(() => {})}}});
  assert.equal(p.webgpu, false);
});
