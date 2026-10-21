#!/usr/bin/env node
// 真实浏览器里的 Web UI 正向探针：暂停后依次按 F3/F4/R，截图中央视口并断言像素变化。
//
// 为什么不是只跑 `probe_webgl2.sh`：那条只证明"能启动/第一帧出来了"，不证明
// "按一个键，画面真的跟着变"。这里用 CDP 的 Input.dispatchKeyEvent + Page.captureScreenshot，
// 把"输入 → 引擎状态 → 画面"这条链闭合。截图只裁中央视口，避开侧栏 FPS/状态文字，
// 否则每帧变化的 FPS 也会让哈希不同（假绿）。
//
// 依赖：Node 22+（全局 WebSocket/fetch）、Chrome/Chromium。由 probe_web_ui.sh 启动。
import { createHash } from "node:crypto";
import { mkdirSync, writeFileSync } from "node:fs";

const cdpPort = process.env.CDP_PORT || "9223";
const outDir = process.env.OUT_DIR || "target/tmp/ui-probe";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function die(msg) {
  console.error(`FAIL probe-web-ui：${msg}`);
  process.exit(1);
}

async function connect() {
  let target;
  for (let i = 0; i < 80; i++) {
    try {
      const list = await (await fetch(`http://127.0.0.1:${cdpPort}/json/list`)).json();
      target = list.find((t) => t.type === "page" && t.webSocketDebuggerUrl);
      if (target) break;
    } catch {
      // Chrome 还没起监听
    }
    await sleep(250);
  }
  if (!target) die("连不上 Chrome 的 CDP（/json/list 没有 page target）");

  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => {
    ws.addEventListener("open", resolve, { once: true });
    ws.addEventListener("error", reject, { once: true });
  });
  let nextId = 1;
  const pending = new Map();
  const events = [];
  ws.addEventListener("message", (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.id && pending.has(msg.id)) {
      const { resolve, reject } = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) reject(new Error(JSON.stringify(msg.error)));
      else resolve(msg.result);
    } else if (msg.method) {
      events.push(msg);
    }
  });
  const send = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const id = nextId++;
      pending.set(id, { resolve, reject });
      ws.send(JSON.stringify({ id, method, params }));
    });
  return { ws, send, events };
}

async function evaluate(send, expression) {
  const r = await send("Runtime.evaluate", {
    expression,
    returnByValue: true,
    awaitPromise: true,
  });
  if (r.exceptionDetails) die(`页面脚本异常：${r.exceptionDetails.text}`);
  return r.result?.value;
}

async function key(send, key, code, vk) {
  const base = {
    key,
    code,
    windowsVirtualKeyCode: vk,
    nativeVirtualKeyCode: vk,
  };
  // 函数键（F3/F4）在 Chrome 里可能被浏览器先接走；`rawKeyDown` 是 CDP 里
  // 最接近"物理按键"的事件类型，普通键也兼容。
  await send("Input.dispatchKeyEvent", { type: "rawKeyDown", ...base });
  await send("Input.dispatchKeyEvent", { type: "keyUp", ...base });
}

async function main() {
  mkdirSync(outDir, { recursive: true });
  const { ws, send, events } = await connect();
  await send("Runtime.enable");
  await send("Page.enable");
  await send("Log.enable");
  await send("Page.bringToFront");

  // 等遮罩收起（第一帧真的画出来了），而不是死等一个固定秒数。
  let hidden = false;
  for (let i = 0; i < 120; i++) {
    hidden = await evaluate(
      send,
      `document.getElementById("overlay")?.classList.contains("hidden") === true`,
    );
    if (hidden) break;
    await sleep(250);
  }
  if (!hidden) die("30 秒内遮罩没有收起（第一帧没画出来）");

  // 先把焦点从输入框挪到视口：`egui_wants_keyboard_input()` 为真时编辑器会
  // 故意让 F3/F4/P 让路（R 走编辑器自己的分支不受影响，所以只测 R 会假绿）。
  // 点一下视口中央即可；不拖动，不会转视角。
  await send("Input.dispatchMouseEvent", {
    type: "mousePressed",
    x: 640,
    y: 400,
    button: "left",
    buttons: 1,
    clickCount: 1,
  });
  await send("Input.dispatchMouseEvent", {
    type: "mouseReleased",
    x: 640,
    y: 400,
    button: "left",
    buttons: 0,
    clickCount: 1,
  });
  await sleep(300);

  await evaluate(
    send,
    `window.__quench3d_keys = [];
     window.addEventListener("keydown", (e) => window.__quench3d_keys.push([e.key, e.code, e.keyCode, e.defaultPrevented]), true);`,
  );
  const uiState = async () =>
    evaluate(send, `document.body.dataset.quench3dUi || "(无 ui_debug 状态)"`);
  const state = async () =>
    `${await uiState()} / last=${await evaluate(
      send,
      `document.body.dataset.quench3dLastKey || "(无)"`,
    )}`;
  console.log(`状态（点击后）：${await state()}`);

  // 编辑器启动就是暂停的（`app.rs` 里 `engine.paused = true`），不要再按 P：
  // 否则会变成"播放"，画面自己动，截图差异就分不清是按键还是物理了。
  if (!String(await state()).includes("paused=true")) {
    die("编辑器启动后不是暂停状态，无法用截图差异判断按键");
  }

  const clip = { x: 300, y: 60, width: 680, height: 600, scale: 1 };
  const snap = async (name) => {
    const r = await send("Page.captureScreenshot", {
      format: "png",
      captureBeyondViewport: false,
      clip,
    });
    const buf = Buffer.from(r.data, "base64");
    writeFileSync(`${outDir}/${name}.png`, buf);
    return createHash("sha256").update(buf).digest("hex");
  };

  const results = [];
  const toggle = async (label, k, code, vk) => {
    const before = await snap(`${label}-before`);
    const beforeUi = await uiState();
    const beforeState = await state();
    await key(send, k, code, vk);
    await sleep(400);
    const after = await snap(`${label}-after`);
    const afterUi = await uiState();
    const afterState = await state();
    console.log(`${label}：${beforeState} → ${afterState}`);
    if (beforeUi === afterUi) {
      die(`${label} 的按键只改了截图、没改状态（假绿）：${beforeState}`);
    }
    results.push({ feature: label, before, after, changed: before !== after });
    await key(send, k, code, vk); // 还原，避免影响下一项
    await sleep(300);
  };

  await toggle("网格 F3", "F3", "F3", 114);
  await toggle("阴影 F4", "F4", "F4", 115);
  await toggle("缩放手柄 R", "r", "KeyR", 82);

  // 运行时错误/panic/GL 能力日志都要当失败（与 probe_webgl2 的判据一致）。
  const logs = events
    .filter((e) => e.method === "Log.entryAdded" || e.method === "Runtime.exceptionThrown")
    .map((e) => JSON.stringify(e.params));
  const fatal = logs.find(
    (s) =>
      /panicked|INDEPENDENT_BLEND|heuristics assumed|wgpu error/i.test(s),
  );
  if (fatal) die(`浏览器日志里出现致命错误：${fatal.slice(0, 500)}`);

  console.log("浏览器收到的 keydown：", JSON.stringify(await evaluate(send, "window.__quench3d_keys")));
  const failed = results.filter((r) => !r.changed);
  if (failed.length) {
    die(
      `按键后画面没变：${failed.map((r) => r.feature).join("、")}（截图在 ${outDir}）`,
    );
  }
  console.log("== Web UI 正向探针 ==");
  for (const r of results) {
    console.log(`PASS ${r.feature}：${r.before.slice(0, 12)} → ${r.after.slice(0, 12)}`);
  }
  console.log(`PASS probe-web-ui（${results.length} 项按键均改变画面；截图 ${outDir}）`);
  ws.close();
}

main().catch((e) => die(e?.stack || String(e)));
