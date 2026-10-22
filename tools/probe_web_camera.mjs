#!/usr/bin/env node
// Web 摄像头预览的 CDP 探针：用 Chrome 的假摄像头打开 `?camera=1`，
// 等第一帧后隔 1.2 秒再截一张；两张图在预览区域必须有运动。
// 由 probe_web_camera.sh 启动 Chrome 与静态服务。
import { mkdirSync, writeFileSync } from "node:fs";

const cdpPort = process.env.CDP_PORT || "9232";
const outDir = process.env.OUT_DIR || "target/tmp/web-camera-probe";
const url = process.env.CAMERA_URL || "http://127.0.0.1:8152/?backend=gl&ui_debug=1&camera=1";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function die(msg) {
  console.error(`FAIL probe-web-camera：${msg}`);
  process.exit(1);
}

async function connect() {
  let target;
  for (let i = 0; i < 80; i++) {
    try {
      const list = await (await fetch(`http://127.0.0.1:${cdpPort}/json/list`)).json();
      target = list.find((t) => t.type === "page" && t.webSocketDebuggerUrl);
      if (target) break;
    } catch {}
    await sleep(250);
  }
  if (!target) die("连不上 Chrome 的 CDP");
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => {
    ws.addEventListener("open", resolve, { once: true });
    ws.addEventListener("error", reject, { once: true });
  });
  let nextId = 1;
  const pending = new Map();
  ws.addEventListener("message", (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.id && pending.has(msg.id)) {
      const { resolve, reject } = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) reject(new Error(JSON.stringify(msg.error)));
      else resolve(msg.result);
    }
  });
  const send = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const id = nextId++;
      pending.set(id, { resolve, reject });
      ws.send(JSON.stringify({ id, method, params }));
    });
  return { ws, send };
}

async function main() {
  mkdirSync(outDir, { recursive: true });
  const { ws, send } = await connect();
  await send("Page.enable");
  await send("Runtime.enable");
  await send("Page.navigate", { url });
  // 等遮罩收起，确保 wasm 第一帧真的画出来了。
  for (let i = 0; i < 120; i++) {
    const r = await send("Runtime.evaluate", {
      expression: `document.getElementById("overlay")?.classList.contains("hidden") === true`,
      returnByValue: true,
    });
    if (r.result?.value) break;
    await sleep(250);
  }
  await sleep(4000);
  const a = await send("Page.captureScreenshot", { format: "png" });
  writeFileSync(`${outDir}/camera-a.png`, Buffer.from(a.data, "base64"));
  await sleep(1200);
  const b = await send("Page.captureScreenshot", { format: "png" });
  writeFileSync(`${outDir}/camera-b.png`, Buffer.from(b.data, "base64"));
  ws.close();
  console.log("PASS 摄像头截图：camera-a.png / camera-b.png");
}

main().catch((e) => die(e.stack || String(e)));
