#!/usr/bin/env node
// 真实浏览器里的 Web UI 正向探针：暂停后依次按 F3/F4/R/F6/F7/F8/P/F5/Tab，
// 截图中央视口并断言状态（多数项还要求像素）变化。
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
  // headless 在多次 reload 后偶尔会把键盘焦点丢给浏览器 UI；焦点模拟保证
  // Shift/Ctrl/F9 这类没有鼠标坐标的键仍然送到页面。
  await send("Emulation.setFocusEmulationEnabled", { enabled: true });

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
  // 没有编辑过的默认场景不该写恢复点（否则每次刷新都会弹"发现自动保存"）。
  const untouched = await evaluate(
    send,
    `localStorage.getItem("quench3d.autosave.scene.v1")`,
  );
  if (untouched !== null) {
    die(`未编辑的默认场景就写了自动保存（${String(untouched).length} 字节）`);
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
  const toggle = async (label, k, code, vk, expectPixels = true) => {
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
    // 有些快捷键只切模式、不立刻挪动画面（例如 Tab 在 Orbit/Fly 之间切），
    // 这时只要求状态变；其余项要求截图也变，否则是"状态在改、画面没跟上"。
    if (expectPixels && before === after) {
      die(`${label} 状态变了但画面没变（假绿）：${beforeState} → ${afterState}`);
    }
    if (expectPixels) {
      results.push({ feature: label, before, after, changed: before !== after });
    }
    await key(send, k, code, vk); // 还原，避免影响下一项
    await sleep(300);
  };

  await toggle("网格 F3", "F3", "F3", 114);
  await toggle("阴影 F4", "F4", "F4", 115);
  await toggle("缩放手柄 R", "r", "KeyR", 82);
  await toggle("无头包围盒 F6", "F6", "F6", 117);
  // F5 只画**已有接触**；默认场景暂停在初始姿态时可能一个接触都没有，
  // 所以只要求状态切换正确，不强迫截图变化（F6/F7 有几何，必须变）。
  await toggle("接触点 F5", "F5", "F5", 116, false);
  await toggle("查询探针 F7", "F7", "F7", 118);
  // F8 只在有播放模型时画骨头；默认场景没有动画，只验状态切换。
  await toggle("骨骼 F8", "F8", "F8", 119, false);
  // 默认场景没有关节，F11 只验状态切换；线段绘制有 Rust 单测。
  await toggle("关节 F11", "F11", "F11", 122, false);
  // 默认场景没有探针；B 只要求显示开关状态变化，线框绘制有 Rust 单测。
  await toggle("场景探针 B", "b", "KeyB", 66, false);
  // 默认场景的亮度足够区分 ACES/Reinhard：要求状态与画面都变。
  await toggle("色调 T", "t", "KeyT", 84);
  // K 只移动 3D 光标/切轴心；默认选中物可能正好在原点，所以只验状态。
  await toggle("3D 光标 K", "k", "KeyK", 75, false);
  // O 只切自动 LOD 开关；默认场景都是低模，不会立刻改画面。
  await toggle("自动 LOD O", "o", "KeyO", 79, false);
  await toggle("暂停 P", "p", "KeyP", 80);
  // Tab 只切相机模式：状态必须变，但不强制中央视口立刻不同。
  await toggle("相机模式 Tab", "Tab", "Tab", 9, false);
  // 浏览器/egui 的 Tab 焦点遍历会把键盘交给控件（`keys_enabled=false`），
  // 这是 egui 的正常焦点行为；探针到此结束，不再用 Tab 还原（会被焦点吃掉）。
  // 关键是上面已经断言：Tab 事件确实到达了 winit，并把 Orbit 切成了 Fly。

  // ---- 自动保存恢复：localStorage 读回 → F9 恢复 → 物体数/恢复点同步变化 ----
  // Web 端没有文件系统，自动保存写到这个版本化键（见 autosave.rs::WEB_KEY）。
  const savedScene = JSON.stringify({
    camera: { position: [4, 3, 7], look_at: [0, 1, 0] },
    objects: [
      {
        name: "自动恢复箱",
        shape: { type: "box", size: [1, 1, 1] },
        position: [0, 1, 0],
        body: "static",
        material: "recovered",
      },
    ],
    materials: { recovered: { color: [0.9, 0.2, 0.1], roughness: 0.5 } },
  });
  await evaluate(
    send,
    `localStorage.setItem("quench3d.autosave.scene.v1", ${JSON.stringify(savedScene)})`,
  );
  await send("Page.reload");
  let reloaded = false;
  for (let i = 0; i < 120; i++) {
    reloaded = await evaluate(
      send,
      `document.getElementById("overlay")?.classList.contains("hidden") === true`,
    );
    if (reloaded) break;
    await sleep(250);
  }
  if (!reloaded) die("刷新后 30 秒内遮罩没有收起（自动保存恢复页没画出来）");
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
  const recoveryState = await uiState();
  if (!recoveryState.includes("recovery=true")) {
    die(`刷新后没有读出 localStorage 里的自动保存：${recoveryState}`);
  }
  const objectsOf = (s) => Number(/objects=(\d+)/.exec(s)?.[1] ?? -1);
  const beforeCount = objectsOf(recoveryState);
  if (!(beforeCount > 1)) {
    die(`恢复前的默认场景物体数异常（应当 >1）：${recoveryState}`);
  }
  const beforeRestore = await snap("autosave-before");
  await key(send, "F9", "F9", 120);
  await sleep(1000);
  const restoredState = await uiState();
  if (restoredState.includes("recovery=true")) {
    die(`F9 没有清掉恢复点：${restoredState}`);
  }
  if (objectsOf(restoredState) !== 1) {
    die(`F9 恢复后物体数应变成 1（场景里只有 1 个箱）：${restoredState}`);
  }
  const afterRestore = await snap("autosave-after");
  if (beforeRestore === afterRestore) {
    die("F9 恢复后画面没有变化（恢复可能只改了状态没装场景）");
  }
  results.push({
    feature: "自动保存 F9",
    before: beforeRestore,
    after: afterRestore,
    changed: true,
  });
  // 恢复后 `clear()` 把计时重置，下一帧应立刻写出新的恢复点（写路径也要测）。
  const written = await evaluate(
    send,
    `localStorage.getItem("quench3d.autosave.scene.v1")`,
  );
  if (!written || !written.includes("自动恢复箱")) {
    die(`恢复后没有重新写出 localStorage 自动保存：${String(written).slice(0, 200)}`);
  }
  console.log(`PASS 自动保存写入：${written.length} 字节`);

  // 再刷新一次：恢复点应该重新出现；F10 丢弃后 localStorage 与状态都要清。
  await send("Page.reload");
  let reloadedAgain = false;
  for (let i = 0; i < 120; i++) {
    reloadedAgain = await evaluate(
      send,
      `document.getElementById("overlay")?.classList.contains("hidden") === true`,
    );
    if (reloadedAgain) break;
    await sleep(250);
  }
  if (!reloadedAgain) die("第二次刷新后遮罩没有收起");
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
  const secondRecovery = await uiState();
  if (!secondRecovery.includes("recovery=true")) {
    die(`重新写出的恢复点在刷新后没有出现：${secondRecovery}`);
  }
  await key(send, "F10", "F10", 121);
  await sleep(500);
  const discarded = await uiState();
  if (discarded.includes("recovery=true")) {
    die(`F10 没有清掉恢复点：${discarded}`);
  }
  const afterDiscard = await evaluate(
    send,
    `localStorage.getItem("quench3d.autosave.scene.v1")`,
  );
  if (afterDiscard !== null) {
    die(`F10 没有删掉 localStorage 恢复点（${String(afterDiscard).length} 字节）`);
  }
  console.log("PASS 自动保存丢弃 F10");

  // ---- 框选：Shift+左键拖动 → 选中数 >0、画面出现选中轮廓 ----
  const selectedOf = (s) => Number(/selected=(\d+)/.exec(s)?.[1] ?? -1);
  // 载入默认场景后可能已经选中 1 个（例如地面），框选是**追加**：只要变多即可。
  const beforeBoxSelected = selectedOf(await uiState());
  const beforeBox = await snap("box-before");
  let boxed = await uiState();
  // 无坐标的修饰键事件在 headless 里偶尔会晚一帧/丢一次；最多重试 3 次，
  // 仍然失败才算 UI 回归。每次只在"选中数没变"时重试，不会重复追加。
  for (let attempt = 0; attempt < 3 && !(selectedOf(boxed) > beforeBoxSelected); attempt++) {
    await send("Input.dispatchKeyEvent", {
      type: "rawKeyDown",
      key: "Shift",
      code: "ShiftLeft",
      windowsVirtualKeyCode: 16,
      nativeVirtualKeyCode: 16,
      modifiers: 8,
    });
    await sleep(300);
    await send("Input.dispatchMouseEvent", {
      type: "mouseMoved",
      x: 380,
      y: 220,
      button: "none",
      buttons: 0,
      modifiers: 8,
    });
    await sleep(120);
    await send("Input.dispatchMouseEvent", {
      type: "mousePressed",
      x: 380,
      y: 220,
      button: "left",
      buttons: 1,
      clickCount: 1,
      modifiers: 8,
    });
    for (const [x, y] of [
      [430, 270],
      [520, 360],
      [650, 470],
      [820, 610],
    ]) {
      await send("Input.dispatchMouseEvent", {
        type: "mouseMoved",
        x,
        y,
        button: "left",
        buttons: 1,
        modifiers: 8,
      });
      await sleep(80);
    }
    await send("Input.dispatchMouseEvent", {
      type: "mouseReleased",
      x: 820,
      y: 610,
      button: "left",
      buttons: 0,
      clickCount: 1,
      modifiers: 8,
    });
    await send("Input.dispatchKeyEvent", {
      type: "keyUp",
      key: "Shift",
      code: "ShiftLeft",
      windowsVirtualKeyCode: 16,
      nativeVirtualKeyCode: 16,
      modifiers: 0,
    });
    await sleep(500);
    boxed = await uiState();
  }
  const afterBoxFail = await snap("box-after-fail");
  if (!(selectedOf(boxed) > beforeBoxSelected)) {
    die(
      `Shift 框选没有追加选中（之前 ${beforeBoxSelected}，之后 ${selectedOf(boxed)}，画面 ${afterBoxFail}）：${boxed}`,
    );
  }
  const afterBox = await snap("box-after");
  if (beforeBox === afterBox) {
    die("框选选中了物体但画面没有出现选中轮廓");
  }
  results.push({
    feature: "框选 Shift+拖动",
    before: beforeBox,
    after: afterBox,
    changed: true,
  });
  console.log(
    `PASS 框选：selected ${beforeBoxSelected} → ${selectedOf(boxed)}`,
  );

  // ---- Ctrl+C / Ctrl+V：内部剪贴板粘贴出物体 ----
  const beforePaste = objectsOf(boxed);
  const ctrlKey = async (type, key, code, vk) =>
    send("Input.dispatchKeyEvent", {
      type,
      key,
      code,
      windowsVirtualKeyCode: vk,
      nativeVirtualKeyCode: vk,
      modifiers: 2,
    });
  let pastedState = await uiState();
  let afterPaste = objectsOf(pastedState);
  for (let attempt = 0; attempt < 3 && !(afterPaste > beforePaste); attempt++) {
    await ctrlKey("rawKeyDown", "Control", "ControlLeft", 17);
    await sleep(120);
    await ctrlKey("rawKeyDown", "c", "KeyC", 67);
    await sleep(120);
    await ctrlKey("keyUp", "c", "KeyC", 67);
    await ctrlKey("keyUp", "Control", "ControlLeft", 17);
    await sleep(350);
    await ctrlKey("rawKeyDown", "Control", "ControlLeft", 17);
    await sleep(120);
    await ctrlKey("rawKeyDown", "v", "KeyV", 86);
    await sleep(120);
    await ctrlKey("keyUp", "v", "KeyV", 86);
    await ctrlKey("keyUp", "Control", "ControlLeft", 17);
    await sleep(700);
    pastedState = await uiState();
    afterPaste = objectsOf(pastedState);
  }
  if (!(afterPaste > beforePaste)) {
    die(`Ctrl+C/V 没有粘贴出新物体（${beforePaste} → ${afterPaste}）：${pastedState}`);
  }
  const afterPasteSnap = await snap("clipboard-after");
  if (afterBox === afterPasteSnap) {
    die("Ctrl+V 增加了物体但画面没有变化");
  }
  results.push({
    feature: "复制粘贴 Ctrl+C/V",
    before: afterBox,
    after: afterPasteSnap,
    changed: true,
  });
  console.log(`PASS 复制粘贴：objects ${beforePaste} → ${afterPaste}`);

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
