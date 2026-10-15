#!/usr/bin/env node
// 检查 Web 产物是否真的可用（不需要浏览器）：
//   1) web/pkg/ 里的 wasm 能被实例化（wasm-bindgen 胶水代码能跑起来）
//   2) web/index.html 里 import 的每个符号，胶水代码都真的导出了
//   3) 胶水代码引用的 wasm 文件名与磁盘上的文件一致
//
// 用法：
//   node tools/check_web.mjs                    # 默认检查 web/pkg 与 web/index.html
//   node tools/check_web.mjs some/dir           # 指定 Web 根目录
//   node tools/check_web.mjs web --call-start   # 额外调用 start()（Node 里必然 panic，输出较吵）
//
// 浏览器里的渲染正确性无法靠它验证（那需要真实 GPU 与浏览器），
// 但它能挡住「打包漏文件 / 页面 import 了不存在的函数 / wasm 损坏」这类问题。

import { readFileSync, existsSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { resolve, join, sep } from "node:path";

const root = resolve(process.argv[2] ?? "web");
const pkg = join(root, "pkg");
const jsPath = join(pkg, "quench3d_web.js");
const wasmPath = join(pkg, "quench3d_web_bg.wasm");
const htmlPath = join(root, "index.html");

let failures = 0;
let warnings = 0;
const ok = (msg) => console.log(`  ✓ ${msg}`);
const warn = (msg) => {
  warnings += 1;
  console.log(`  ! ${msg}`);
};
const bad = (msg) => {
  failures += 1;
  console.error(`  ✗ ${msg}`);
};

console.log(`检查 Web 产物：${root}`);

for (const f of [jsPath, wasmPath, htmlPath]) {
  if (!existsSync(f)) {
    bad(`缺少文件 ${f}（先跑 tools/build_web.sh）`);
  }
}
if (failures > 0) process.exit(1);

const js = readFileSync(jsPath, "utf8");
const wasmBytes = readFileSync(wasmPath);
ok(`胶水 ${(js.length / 1024).toFixed(1)} KB，wasm ${(wasmBytes.length / 1024).toFixed(1)} KB`);

// 2.5) wasm 里不许残留「这个平台不支持」的 panic 字符串
//
// 这类问题 `cargo check --target wasm32` 发现不了：编译通过、而是在**浏览器里
// 第一次跑到就 panic**（曾经就是这样：引擎里用了 std::time::Instant，
// wasm32-unknown-unknown 上它直接 panic「time not implemented on this platform」，
// 页面白屏）。panic 消息是静态字符串、会被编进 wasm，所以在产物里扫一遍就能挡住。
const BANNED = [
  [
    "time not implemented on this platform",
    "有代码在 wasm 上调用了 std::time::Instant/SystemTime，运行期必然 panic",
  ],
  [
    "thread not implemented on this platform",
    "有代码在 wasm 上起了线程，应该用 cfg(target_arch = \"wasm32\") 排除",
  ],
];
{
  const text = wasmBytes.toString("latin1");
  // 符号名里的「谁在用」：字符串无法归因，但名字段能看出是自有代码还是依赖。
  const symbols = [...new Set(text.match(/[A-Za-z0-9_:<>$.]{10,}/g) ?? [])];
  const own = (api) =>
    symbols.filter((s) => s.includes(api) && /quench3d/i.test(s)).length;
  for (const [needle, hint] of BANNED) {
    if (!text.includes(needle)) {
      ok(`wasm 里没有「${needle}」`);
      continue;
    }
    const api = needle.startsWith("time") ? "Instant" : "thread";
    if (own(api) > 0) {
      bad(`wasm 里残留 panic 字符串「${needle}」，且有 quench3d 自己的符号在用 ${api}：${hint}`);
    } else {
      // 依赖里可能也带这条路径（本项目里 std::time 的符号全部来自依赖），
      // 无法从产物判断它是否可达，所以只提示；真正的硬判定是下面的 clock_probe()。
      warn(
        `wasm 里含「${needle}」，但 ${api} 相关符号没有一个来自 quench3d —— ` +
          `多半是依赖里未走到的分支；自有时间路径由 clock_probe() 实测`,
      );
    }
  }
}

// 2.7) WebGL2 回退必须在产物里，且页面必须在**碰画布之前**探好后端
//
// 两个真实故障（都发生过，且都不报编译错）：
//   a) wasm 里没编进 wgpu 的 GL 后端时，浏览器里只剩 WebGPU 一条路：
//      没有 WebGPU 适配器的机器（Linux Chrome 没开 Vulkan、软件光栅环境）
//      直接 `No available adapters` —— 而错误信息里 supported_backends 只有
//      BROWSER_WEBGPU，看起来像"浏览器不支持"，其实是产物少编了后端。
//      GL 后端的 web-sys 类型名会静态编进 wasm，扫一遍就能挡住。
//   b) 顺序：wgpu 只要看到请求里有 BROWSER_WEBGPU 且 navigator.gpu 存在，就只建
//      WebGPU 上下文；而**对着画布要过 webgpu 上下文后，同一张画布的
//      getContext("webgl2") 会返回 null**（一张画布只能有一种上下文，不可逆）。
//      所以必须先用 navigator.gpu.requestAdapter() 探（不碰画布），把结论写进
//      globalThis.__quench3d_backend，再 start()。
//
// 浏览器内真正的渲染仍然只能靠真实浏览器验证。本机可用的复现命令（软件光栅，
// 专门制造"没有 WebGPU"的环境）：
//   python3 -m http.server 8231 --directory web &
//   google-chrome --headless=new --no-sandbox --enable-unsafe-swiftshader \
//     --use-gl=angle --use-angle=swiftshader --virtual-time-budget=40000 \
//     --screenshot=/tmp/web.png http://127.0.0.1:8231/
// 控制台应打印「后端选择：gl（navigator.gpu 存在但没有适配器…）」并出「第一帧已渲染」。
{
  const text = wasmBytes.toString("latin1");
  const GL_MARKERS = ["WebGl2RenderingContext", "EXT_color_buffer_float"];
  const missing = GL_MARKERS.filter((m) => !text.includes(m));
  if (missing.length === 0) {
    ok("wasm 里含 WebGL2 回退（GL 后端已编进产物）");
  } else {
    bad(
      `wasm 里找不到 WebGL2 回退的痕迹（缺 ${missing.join("、")}）：` +
        "浏览器里会只剩 WebGPU，没有 WebGPU 适配器的机器直接白屏。" +
        "检查 quench3d-render 的 Cargo.toml：wasm32 目标是否开了 wgpu 的 `webgl` feature",
    );
  }

  const html2 = readFileSync(htmlPath, "utf8");
  if (!html2.includes("__quench3d_backend")) {
    bad(
      "index.html 没有把后端探测结果写进 globalThis.__quench3d_backend：" +
        "引擎只能按默认顺序（WebGPU → WebGL2）盲试，画布会被 webgpu 上下文占用，回退失效",
    );
  } else {
    const assignAt = html2.indexOf("__quench3d_backend");
    const callMatch = /^[ \t]*start\(\);/m.exec(html2);
    const callAt = callMatch ? callMatch.index : -1;
    if (callAt < 0) {
      warn("index.html 里找不到 `start();` 调用，无法核对后端探测与启动的先后顺序");
    } else if (assignAt < callAt) {
      ok("后端探测在 start() 之前完成");
    } else {
      bad(
        "index.html 里 __quench3d_backend 的赋值在 start() 之后：" +
          "引擎会先按默认顺序去要 WebGPU 上下文，画布随即被占用，WebGL2 回退再也拿不到",
      );
    }
  }
}

// 2.8) 产物新鲜度：源码比 pkg 新 → 这个 pkg 是旧的，后面全是自欺欺人
{
  const { readdirSync, statSync, readFileSync } = await import("node:fs");
  const { dirname } = await import("node:path");
  const repoRoot = dirname(root);
  // 构建戳告诉我们这个包是哪个 example 编出来的；只有那个示例文件与该示例相关，
  // 其它示例改了不该影响它（否则每加一个示例都会误报「产物是旧的」）。
  let example = null;
  try {
    const stamp = readFileSync(join(root, "pkg", "build_stamp.txt"), "utf8");
    example = /example=(\S+)/.exec(stamp)?.[1] ?? null;
  } catch {}
  let newest = 0;
  let newestFile = "";
  const walk = (dir) => {
    let entries;
    try {
      entries = readdirSync(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const e of entries) {
      if (e.name === "target" || e.name === "node_modules" || e.name.startsWith(".")) continue;
      // CLI 是 native 二进制，不参与 wasm 产物（它依赖门面，反过来不成立）：
      // 不跳过的话，改一行 CLI 就会误报「web/pkg 是旧的」。
      if (e.isDirectory() && e.name === "quench3d-cli") continue;
      const full = join(dir, e.name);
      if (e.isDirectory()) walk(full);
      else if (e.name.endsWith(".rs") || e.name.endsWith(".toml")) {
        // 只有能进产物的源码才算：其它示例、测试、基准都不影响这个 wasm。
        const isExample = full.includes(`${sep}examples${sep}`);
        const isTest = full.includes(`${sep}tests${sep}`);
        const isBench = full.includes(`${sep}benches${sep}`);
        if (isTest || isBench) continue;
        if (isExample && !(example && e.name === `${example}.rs`)) continue;
        const t = statSync(full).mtimeMs;
        if (t > newest) {
          newest = t;
          newestFile = full;
        }
      }
    }
  };
  walk(join(repoRoot, "crates"));
  for (const f of ["Cargo.toml", "Cargo.lock"]) {
    try {
      const t = statSync(join(repoRoot, f)).mtimeMs;
      if (t > newest) {
        newest = t;
        newestFile = f;
      }
    } catch {}
  }
  const pkgTime = statSync(wasmPath).mtimeMs;
  const detail =
    `最近改动 ${newestFile} @ ${new Date(newest).toISOString()}，` +
    `产物 @ ${new Date(pkgTime).toISOString()}`;
  if (!example) {
    warn(`没有构建戳（web/pkg/build_stamp.txt），无法精确判断新鲜度：${detail}`);
  } else if (newest > pkgTime + 1000) {
    bad(`web/pkg 是旧的：${detail}；先跑 tools/build_web.sh ${example}`);
  } else {
    ok(`产物不比源码旧（example=${example}）`);
  }
}

// 3) 胶水引用的 wasm 文件名
const referenced = [...js.matchAll(/([A-Za-z0-9_]+\.wasm)/g)].map((m) => m[1]);
if (referenced.length === 0) {
  bad("胶水代码里找不到 wasm 文件名");
} else if (!referenced.includes("quench3d_web_bg.wasm")) {
  bad(`胶水引用的 wasm 名不符：${[...new Set(referenced)].join(", ")}`);
} else {
  ok("胶水代码引用的 wasm 文件名匹配");
}

// 2) 页面 import 的符号
const html = readFileSync(htmlPath, "utf8");
// `import init, { start } from "..."`：init 是**默认导出**，其余是具名导出
const staticImports = [
  ...html.matchAll(/import\s*([A-Za-z0-9_$]*)\s*,?\s*\{([^}]*)\}\s*from\s*["']([^"']+)["']/g),
];
let defaultBinding = null;
const namedFromGlue = new Set();
for (const [, def, names, spec] of staticImports) {
  if (!spec.includes("quench3d_web")) continue;
  if (def) defaultBinding = def;
  for (const n of names.split(",")) {
    const t = n.trim().split(/\s+as\s+/)[0].trim();
    if (t) namedFromGlue.add(t);
  }
}
if (!defaultBinding && namedFromGlue.size === 0) {
  bad("index.html 里没有找到引用 quench3d_web 的 import");
} else {
  const exported = await import(pathToFileURL(jsPath).href);
  if (defaultBinding) {
    if (typeof exported.default === "function") {
      ok(`页面默认导入的 ${defaultBinding}（wasm-bindgen 的 init）可调用`);
    } else {
      bad(`页面默认导入了 ${defaultBinding}，但胶水没有默认导出函数`);
    }
  }
  for (const name of namedFromGlue) {
    if (typeof exported[name] === "function") {
      ok(`页面要的 ${name}() 在胶水里有导出`);
    } else if (name in exported) {
      ok(`页面要的 ${name} 在胶水里有导出`);
    } else {
      bad(`页面 import 了 ${name}，但胶水没有导出它`);
    }
  }
  // 1) 真正实例化 wasm：把编译好的模块交给胶水，它会自己接好 wasm 的 imports
  try {
    const module = await WebAssembly.compile(wasmBytes);
    const wasm = await exported.default({ module_or_path: module });
    const count = wasm && typeof wasm === "object" ? Object.keys(wasm).length : 0;
    if (count > 0) {
      ok(`wasm 实例化成功（胶水暴露 ${count} 个 Rust 导出）`);
    } else {
      bad("init() 没有返回 wasm 导出对象");
    }
    // 时钟探针：主动读一次引擎时钟。这是唯一能在 Node 里跑到的
    // 「曾经会 panic 的时间路径」——比静态扫描更硬。
    if (typeof exported.clock_probe === "function") {
      const t0 = exported.clock_probe();
      const t1 = exported.clock_probe();
      if (Number.isFinite(t0) && Number.isFinite(t1) && t1 >= t0) {
        ok(`clock_probe() 可调用：${t0.toFixed(3)} → ${t1.toFixed(3)}（单调不减）`);
      } else {
        bad(`clock_probe() 返回了异常值：${t0} → ${t1}`);
      }
    } else {
      bad("胶水没有导出 clock_probe()：无法验证 wasm 上的时间实现（示例应当导出它）");
    }

    // 模型加载探针：Web 端没有文件系统，两条路的**行为**都要对。
    //   · 按路径加载 → 必须给出能照着做的报错（提到 load_model_from_bytes）
    //   · 从字节加载 → 必须真的解析出几何（这才是 Web 端加载模型的正路）
    if (typeof exported.model_path_probe === "function") {
      const msg = exported.model_path_probe();
      if (msg.includes("load_model_from_bytes")) {
        ok(`按路径加载模型给出可操作报错：${msg.slice(0, 46)}…`);
      } else {
        bad(`Web 端按路径加载模型的报错没有指向 load_model_from_bytes：${msg}`);
      }
    } else {
      bad("胶水没有导出 model_path_probe()：无法验证 Web 端的模型加载诊断");
    }
    if (typeof exported.model_bytes_probe === "function") {
      const msg = exported.model_bytes_probe();
      if (msg.startsWith("ok") && msg.includes("triangles=1")) {
        ok(`从字节加载模型可用：${msg}`);
      } else {
        bad(`Web 端从字节加载模型失败：${msg}`);
      }
    } else {
      bad("胶水没有导出 model_bytes_probe()：无法验证 Web 端能否加载模型");
    }

    // 浏览器里才有的 API 在 Node 里必然缺失；默认不调用 start()，避免刷屏
    if (process.argv.includes("--call-start")) {
      try {
        exported.start();
        ok("start() 在 Node 上直接返回（没有触碰浏览器 API）");
      } catch (e) {
        ok(`start() 在 Node 上按预期报错（缺浏览器环境）：${String(e).slice(0, 60)}…`);
      }
    }
  } catch (e) {
    bad(`wasm 实例化失败：${e}`);
  }
}

if (failures > 0) {
  console.error(`\n检查失败：${failures} 项`);
  process.exit(1);
}
console.log(
  `\nWeb 产物检查通过${warnings > 0 ? `（${warnings} 条提示）` : ""}` +
    "（浏览器内的渲染仍需真实浏览器 + GPU 验证）",
);
