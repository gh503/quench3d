#!/usr/bin/env node
// 真实浏览器（Chrome/Chromium）里的 wasm 基准：
//   1. 起一个只读的 web/ 静态服务器；
//   2. 用 headless Chrome 打开 web/robot.html?bench=1；
//   3. 从 DevTools 的 /json 里读页面标题（页面把结果写进 title）；
//   4. 断言 1000 步物理 < 2 s，打印 full JSON。
// 这是“浏览器 wasm + Canvas 预览”的数字，不是 wgpu 全渲染帧率；后者仍需真 GPU 浏览器。
import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { readFile, rm, mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { extname, join, normalize } from "node:path";

const root = new URL("../web/", import.meta.url).pathname;
const mime = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".wasm": "application/wasm",
  ".json": "application/json; charset=utf-8",
  ".png": "image/png",
};
const server = createServer(async (req, res) => {
  try {
    const path = normalize(decodeURIComponent(new URL(req.url, "http://x").pathname));
    if (path.includes("..")) throw new Error("bad path");
    const file = join(root, path === "/" ? "index.html" : path);
    const body = await readFile(file);
    res.writeHead(200, { "content-type": mime[extname(file)] ?? "application/octet-stream" });
    res.end(body);
  } catch {
    res.writeHead(404).end("not found");
  }
});
await new Promise((ok) => server.listen(0, "127.0.0.1", ok));
const port = server.address().port;
const chrome = process.env.CHROME ?? "google-chrome";
const profile = await mkdtemp(join(tmpdir(), "quench3d-chrome-"));
const debugPort = 9300 + (process.pid % 300);
const child = spawn(
  chrome,
  [
    "--headless=new",
    "--no-sandbox",
    "--disable-gpu",
    `--user-data-dir=${profile}`,
    `--remote-debugging-port=${debugPort}`,
    `http://127.0.0.1:${port}/robot.html?bench=1`,
  ],
  { stdio: "ignore" },
);
const sleep = (ms) => new Promise((ok) => setTimeout(ok, ms));
let title = null;
for (let i = 0; i < 60; i++) {
  await sleep(500);
  try {
    const tabs = await (await fetch(`http://127.0.0.1:${debugPort}/json`)).json();
    const tab = tabs.find((t) => t.url?.includes("robot.html?bench=1"));
    if (tab?.title?.startsWith("bench-ok ")) {
      title = tab.title.slice("bench-ok ".length);
      break;
    }
  } catch {
    // Chrome 还没起来 / 端口还没监听
  }
}
child.kill("SIGKILL");
server.close();
await new Promise((ok) => {
  child.once("exit", ok);
  setTimeout(ok, 2000);
});
await rm(profile, { recursive: true, force: true, maxRetries: 5 }).catch(() => {});
if (!title) {
  console.error("FAIL browser-bench（没有等到页面结果；Chrome 路径可用 CHROME=... 指定）");
  process.exit(1);
}
const r = JSON.parse(
  title.replaceAll("&quot;", '"').replaceAll("&amp;", "&").replaceAll("&lt;", "<").replaceAll("&gt;", ">"),
);
console.log(
  `browser wasm 物理 1000 步 ${r.physics_1000_steps_ms} ms（${r.physics_ms_per_step} ms/步；` +
    `${(1000 / r.physics_ms_per_step).toFixed(0)} fps 纯物理）`,
);
console.log(`Canvas 预览 120 帧 ${r.canvas_120_frames_ms} ms（${r.canvas_ms_per_frame} ms/帧，CPU 绘制，不含 vsync）`);
if (!(r.physics_1000_steps_ms > 0 && r.physics_1000_steps_ms < 2000)) {
  console.error(`FAIL browser-bench: 1000 步 ${r.physics_1000_steps_ms} ms 不在 (0, 2000)`);
  process.exit(1);
}
console.log("PASS browser-bench");
