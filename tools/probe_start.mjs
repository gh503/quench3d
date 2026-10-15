#!/usr/bin/env node
// 在 Node 里重放 Web 入口 `start()`，把**真正的 panic 原因**打出来。
//
// 为什么需要它：浏览器里 Web 端一白屏，控制台往往只看到
// `RuntimeError: unreachable`；真正的原因（Rust 的 panic 消息）需要
// 打开 wasm 的 panic hook 才能看到。这个脚本在 Node 里 import 胶水、
// 实例化 wasm、调用 `start()`，并把 `console.error` 的 panic 输出捞出来。
//
// 判读方法：
//   * `time not implemented on this platform` → wasm 上用了 std 的时间 API
//     （走 `quench3d::clock` / `set_time_source`，别直接用 `std::time::Instant`）；
//   * `only callable from inside the Window` → 正常现象，Node 里没有浏览器环境；
//   * 其它消息 → 按消息里的文件名/行号去查。
//
// 用法：
//   node tools/probe_start.mjs            # 默认 web/pkg
//   node tools/probe_start.mjs some/dir

import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..", process.argv[2] ?? "web");
const pkg = join(root, "pkg");

const panics = [];
const origError = console.error;
console.error = (...args) => panics.push(args.map(String).join(" "));

let exported;
try {
  exported = await import(pathToFileURL(join(pkg, "quench3d_web.js")).href);
} catch (e) {
  console.error = origError;
  console.log(`胶水加载失败：${e}`);
  process.exit(1);
}

const module = await WebAssembly.compile(readFileSync(join(pkg, "quench3d_web_bg.wasm")));
try {
  const wasm = await exported.default({ module_or_path: module });
  // 时钟探针：这条路径在 Node 里也能跑，正是浏览器里曾经 panic 的那条
  if (typeof exported.clock_probe === "function") {
    console.error = origError;
    console.log(`clock_probe() = ${exported.clock_probe()}（能返回就说明时间实现可用）`);
    console.error = (...args) => panics.push(args.map(String).join(" "));
  }
  try {
    const started = wasm.start ? wasm.start() : exported.start?.();
    if (started && typeof started.then === "function") await started;
    console.error = origError;
    console.log("start() 正常返回（不该发生：Node 里没有浏览器环境）");
  } catch (e) {
    console.error = origError;
    console.log(`start() 抛出：${String(e).split("\n")[0]}`);
  }
} catch (e) {
  console.error = origError;
  console.log(`init() 抛出：${String(e).split("\n")[0]}`);
}

console.log("--- panic 输出 ---");
console.log(panics.join("\n").trim().slice(0, 1200) || "（没有 panic 输出）");
