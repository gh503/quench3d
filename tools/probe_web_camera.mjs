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

  // 外部姿态注入：33 点画成一条竖直/交错骨架，颜色选洋红以区别于假摄像头的绿。
  const injected = await send("Runtime.evaluate", {
    expression: `(() => {
      if (typeof window.quench3dCameraPose !== "function") return "missing";
      const landmarks = Array.from({ length: 33 }, (_, i) => ({
        x: 0.15 + 0.7 * (i % 2),
        y: 0.08 + 0.84 * (i / 32),
        visibility: 1.0,
      }));
      window.quench3dCameraPose(JSON.stringify({ present: true, confidence: 0.9, landmarks }));
      return "ok";
    })()`,
    returnByValue: true,
  });
  if (injected.result?.value !== "ok") {
    die(`页面没有暴露 window.quench3dCameraPose（得到 ${JSON.stringify(injected.result)}）`);
  }
  await sleep(400);
  const poseState = await send("Runtime.evaluate", {
    expression: `document.body.dataset.quench3dUi || ""`,
    returnByValue: true,
  });
  const stateText = poseState.result?.value || "";
  if (!/camera_pose=true/.test(stateText)) {
    die(`注入姿态后 data-quench3d-ui 没有 camera_pose=true：${stateText}`);
  }
  const b = await send("Page.captureScreenshot", { format: "png" });
  writeFileSync(`${outDir}/camera-b.png`, Buffer.from(b.data, "base64"));

  // 只注入 3D 世界点（landmarks 为空）：2D 骨架消失，只留视口里的 3D 骨架。
  const injectedWorld = await send("Runtime.evaluate", {
    expression: `(() => {
      const world = Array.from({ length: 33 }, (_, i) => ({
        x: -0.30 + 0.60 * (i % 2),
        y: 0.90 - 1.20 * (i / 32),
        z: 0.0,
        visibility: 1.0,
      }));
      window.quench3dCameraPose(JSON.stringify({ present: true, confidence: 0.9, landmarks: [], world }));
      return "ok";
    })()`,
    returnByValue: true,
  });
  if (injectedWorld.result?.value !== "ok") die("3D 世界点注入失败");
  await sleep(400);
  const worldState = await send("Runtime.evaluate", {
    expression: `document.body.dataset.quench3dUi || ""`,
    returnByValue: true,
  });
  if (!/camera_world=true/.test(worldState.result?.value || "")) {
    die(`注入世界点后没有 camera_world=true：${worldState.result?.value}`);
  }
  const c = await send("Page.captureScreenshot", { format: "png" });
  writeFileSync(`${outDir}/camera-c.png`, Buffer.from(c.data, "base64"));

  // 清空后再确认状态落回 false —— 反向也走同一条 API。
  await send("Runtime.evaluate", {
    expression: `window.quench3dCameraPoseClear()`,
    returnByValue: true,
  });
  await sleep(200);
  const cleared = await send("Runtime.evaluate", {
    expression: `document.body.dataset.quench3dUi || ""`,
    returnByValue: true,
  });
  if (/camera_pose=true|camera_world=true/.test(cleared.result?.value || "")) {
    die(`清空姿态后 camera_pose/camera_world 仍为 true：${cleared.result?.value}`);
  }
  ws.close();
  console.log("PASS 摄像头截图与外部姿态注入：camera-a/b/c.png");
}

main().catch((e) => die(e.stack || String(e)));
