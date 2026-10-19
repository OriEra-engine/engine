// Kept independent from DOM rendering for workers and deterministic tests.
// No UA sniffing: a browser on a phone remains a browser.
export async function probeBrowser(host = globalThis) {
  const nav = host.navigator || {};
  const result = {
    webgpu: false,
    touch: Number(nav.maxTouchPoints || 0) > 0,
    keyboardMouse: false,
    webtransport: host.isSecureContext === true && typeof host.WebTransport === "function",
  };
  try {
    result.keyboardMouse = host.matchMedia?.("(any-pointer: fine)").matches === true;
  } catch { /* unavailable media queries are not capabilities */ }
  if (host.isSecureContext === true && typeof nav.gpu?.requestAdapter === "function") {
    // API presence alone is insufficient: software policy/drivers may deny it.
    // Bound startup even if a driver never resolves requestAdapter().
    let timer;
    try {
      const adapter = await Promise.race([
        nav.gpu.requestAdapter(),
        new Promise(resolve => { timer = setTimeout(() => resolve(null), 1500); }),
      ]);
      result.webgpu = adapter != null;
    } catch { /* null/rejected adapters fall back to non-WebGPU paths */ }
    finally { clearTimeout(timer); }
  }
  return result;
}
