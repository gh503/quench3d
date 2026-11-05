#!/usr/bin/env node
// Run AFTER rebuilding web_client: CHROME=/usr/bin/google-chrome node tools/probe_web_camera_activation.mjs
// Node 22+ native fetch/WebSocket, Python 3; no npm packages. Only Chrome fake devices are tested.
// Owns its loopback HTTP server and Chrome, retains every profile/evidence file, never deletes files.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createInterface } from 'node:readline';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
if (process.argv.includes('--help')) {
  console.log('After tools/build_web.sh web_client completes:\n  CHROME=/usr/bin/google-chrome node tools/probe_web_camera_activation.mjs\nOptional: PYTHON=python3 WEB_ROOT=/absolute/built/web\nRequires ui_debug egui text rectangles. Tests fake camera only. Evidence and profiles retained under target/tmp/round225-client-config-camera/<unique>.');
  process.exit(0);
}
const webRoot = resolve(process.env.WEB_ROOT || join(root, 'web'));
const base = join(root, 'target/tmp/round225-client-config-camera');
mkdirSync(base, { recursive: true });
const out = mkdtempSync(join(base, 'activation-'));
const profile = join(out, 'chrome-profile');
mkdirSync(profile);
const evidence = { probe: 'camera-activation', fakeDeviceOnly: true, physicalCameraTested: false,
  webRoot, out, stages: [], clicks: [], runtimeExceptions: [], children: [], hashes: {} };
const children = [];
let ws;
let send;
let cleaning;
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const hash = path => createHash('sha256').update(readFileSync(path)).digest('hex');

async function until(fn, label, timeout = 30000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) {
    const value = await fn();
    if (value) return value;
    await sleep(100);
  }
  throw new Error(`timeout: ${label}`);
}
function child(program, args, name) {
  const proc = spawn(program, args, { cwd: root, detached: true, stdio: ['pipe', 'pipe', 'pipe'] });
  children.push(proc);
  evidence.children.push({ name, pid: proc.pid, program, args });
  proc.on('error', error => { proc.spawnError = error; });
  for (const [stream, suffix] of [[proc.stdout, 'stdout'], [proc.stderr, 'stderr']]) {
    stream.on('data', data => writeFileSync(join(out, `${name}.${suffix}.log`), data, { flag: 'a' }));
  }
  return proc;
}
function firstLine(proc) {
  return new Promise((resolve, reject) => {
    const lines = createInterface({ input: proc.stdout });
    const timer = setTimeout(() => { lines.close(); reject(new Error('server readiness timeout')); }, 10000);
    lines.once('line', line => { clearTimeout(timer); lines.close(); resolve(line); });
    proc.once('error', error => { clearTimeout(timer); reject(error); });
    proc.once('exit', code => { clearTimeout(timer); reject(new Error(`server exited before readiness: ${code}`)); });
  });
}
async function connect(url) {
  ws = new WebSocket(url);
  await new Promise((resolve, reject) => {
    ws.addEventListener('open', resolve, { once: true });
    ws.addEventListener('error', reject, { once: true });
  });
  let next = 1;
  const pending = new Map();
  ws.addEventListener('message', event => {
    const message = JSON.parse(event.data);
    if (pending.has(message.id)) {
      const item = pending.get(message.id);
      pending.delete(message.id);
      clearTimeout(item.timer);
      message.error ? item.reject(new Error(JSON.stringify(message.error))) : item.resolve(message.result);
    }
    if (message.method === 'Runtime.exceptionThrown') evidence.runtimeExceptions.push(message.params);
  });
  ws.addEventListener('close', () => {
    for (const item of pending.values()) { clearTimeout(item.timer); item.reject(new Error('CDP closed')); }
    pending.clear();
  });
  return (method, params = {}) => new Promise((resolve, reject) => {
    const id = next++;
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`CDP timeout: ${method}`)); }, 30000);
    pending.set(id, { resolve, reject, timer });
    ws.send(JSON.stringify({ id, method, params }));
  });
}
async function evaluate(expression) {
  const reply = await send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
  if (reply.exceptionDetails) throw new Error(JSON.stringify(reply.exceptionDetails));
  return reply.result?.value;
}

// Observe the real API before wasm starts. Do not manufacture streams or invoke camera APIs ourselves.
const instrument = `(() => {
  const retained = [];
  const calls = [];
  const videos = [];
  const create = Document.prototype.createElement;
  Document.prototype.createElement = function(...args) {
    const element = Reflect.apply(create, this, args);
    if (String(args[0]).toLowerCase() === 'video') videos.push(element);
    return element;
  };
  const native = navigator.mediaDevices.getUserMedia;
  navigator.mediaDevices.getUserMedia = function(...args) {
    const call = { constraints: args[0], requestedAt: performance.now(), status: 'pending' };
    calls.push(call);
    return Reflect.apply(native, this, args).then(stream => {
      retained.push(stream);
      call.status = 'resolved';
      call.streamId = stream.id;
      return stream;
    }, error => {
      call.status = 'rejected'; call.error = String(error); throw error;
    });
  };
  Object.defineProperty(window, '__cameraActivationProbe', { value: {
    snapshot() { return { calls: calls.map(c => ({...c})), streams: retained.map(stream => ({
      id: stream.id, active: stream.active, tracks: stream.getTracks().map(track => ({
        id: track.id, kind: track.kind, label: track.label, readyState: track.readyState,
        enabled: track.enabled, settings: track.getSettings()
      }))
    })), videos: videos.map(v => ({ width: v.videoWidth, height: v.videoHeight,
      currentTime: v.currentTime, readyState: v.readyState, paused: v.paused })) }; }
  }});
})();`;
const snapshot = () => evaluate('window.__cameraActivationProbe?.snapshot()');

// Text metadata is observational: rectangles come from egui's actual rendered text shapes.
async function uiTexts() {
  return evaluate(`(() => {
    const raw = document.body?.getAttribute('data-quench3d-camera-controls');
    if (!raw) return null;
    const data = JSON.parse(raw);
    return Array.isArray(data) ? data : data.texts;
  })()`);
}
async function findText(names, timeout = 10000) {
  return until(async () => {
    const texts = await uiTexts();
    return texts?.find(item => names.includes(item.text));
  }, `rendered egui text ${names.join(' / ')} (requires data-quench3d-camera-controls metadata)`, timeout);
}
async function clickText(names) {
  const item = await findText(names);
  const rect = item.rect;
  assert.ok(Array.isArray(rect) && rect.length === 4 && rect.every(Number.isFinite), JSON.stringify(item));
  const [x0, y0, x1, y1] = rect;
  const x = (x0 + x1) / 2, y = (y0 + y1) / 2;
  const viewport = await evaluate('({width:innerWidth,height:innerHeight})');
  assert.ok(x1 > x0 && y1 > y0 && x >= 0 && y >= 0 && x < viewport.width && y < viewport.height, `text outside viewport: ${JSON.stringify(item)}`);
  evidence.clicks.push({ text: item.text, rect, x, y, clickCount: 1 });
  await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y });
  await sleep(120);
  await send('Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button: 'left', buttons: 1, clickCount: 1 });
  await sleep(80);
  await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x, y, button: 'left', buttons: 0, clickCount: 1 });
}
async function capture(name) {
  const shot = await send('Page.captureScreenshot', { format: 'png' });
  const filename = `${name}.png`;
  writeFileSync(join(out, filename), Buffer.from(shot.data, 'base64'));
  return filename; // Evidence only: never use screenshot pixels to choose controls or infer success.
}
async function load(url) {
  const navigation = await send('Page.navigate', { url });
  assert.ok(!navigation.errorText, navigation.errorText);
  await until(() => evaluate(`location.href === ${JSON.stringify(url)} && document.getElementById('overlay')?.classList.contains('hidden') === true && !!document.body.dataset.quench3dUi && !!window.__cameraActivationProbe`), 'first rendered editor frame', 90000);
  await findText(['视图']);
}
async function live(label) {
  const value = await until(async () => {
    const value = await snapshot();
    const rejected = value?.calls.find(c => c.status === 'rejected');
    assert.ok(!rejected, JSON.stringify(rejected));
    return value?.streams.length === 1 && value.streams[0].tracks.some(t => t.kind === 'video' && t.readyState === 'live') &&
      value.videos.some(v => v.width > 0 && v.height > 0 && v.currentTime > 0) && value;
  }, `${label}: real fake-device stream and advancing video`, 30000);
  assert.equal(value.calls.length, 1, `${label}: exactly one real getUserMedia request`);
  assert.equal(value.calls[0].status, 'resolved');
  const time = Math.max(...value.videos.map(v => v.currentTime));
  const progressed = await until(async () => {
    const next = await snapshot();
    return next.videos.some(v => v.currentTime > time + 0.05) && next;
  }, `${label}: video time advances`);
  assert.equal(progressed.calls.length, 1, `${label}: no duplicate request while preview runs`);
  return progressed;
}
async function stop(label) {
  await clickText(['停止摄像头', '停止']);
  const value = await until(async () => {
    const value = await snapshot();
    return value.streams.length > 0 && value.streams.every(s => s.tracks.length > 0 && s.tracks.every(t => t.readyState === 'ended')) && value;
  }, `${label}: UI stop ends all retained stream tracks`);
  assert.equal(value.calls.length, 1, 'stop must not request another stream');
  await findText(['开启摄像头', '启用摄像头', '启动摄像头', '点击启用']);
  evidence.stages.push({ stage: label, snapshot: value, ui: await evaluate('document.body.dataset.quench3dUi'), screenshot: await capture(label) });
}
async function main() {
  for (const [name, path] of Object.entries({ probe: fileURLToPath(import.meta.url), html: join(webRoot, 'index.html'), js: join(webRoot, 'pkg/quench3d_web.js'), wasm: join(webRoot, 'pkg/quench3d_web_bg.wasm') })) {
    evidence.hashes[name] = { path, sha256: hash(path) };
  }
  const python = `import functools, http.server, json, sys\nclass Handler(http.server.SimpleHTTPRequestHandler):\n    def end_headers(self):\n        self.send_header('Cache-Control', 'no-store')\n        super().end_headers()\nserver = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Handler, directory=sys.argv[1]))\nprint(json.dumps({'url': 'http://127.0.0.1:%d/' % server.server_port}), flush=True)\nserver.serve_forever()\n`;
  const server = child(process.env.PYTHON || 'python3', ['-u', '-c', python, webRoot], 'server');
  const ready = JSON.parse(await firstLine(server));
  assert.match(ready.url, /^http:\/\/127\.0\.0\.1:\d+\/$/);
  evidence.origin = ready.url;
  const chrome = child(process.env.CHROME || '/usr/bin/google-chrome', [
    '--headless=new', '--no-sandbox', '--disable-dev-shm-usage', '--use-angle=swiftshader', '--enable-unsafe-swiftshader',
    '--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream', '--autoplay-policy=no-user-gesture-required',
    '--force-device-scale-factor=1', '--force-renderer-accessibility', '--remote-debugging-address=127.0.0.1',
    '--remote-debugging-port=0', `--user-data-dir=${profile}`, '--window-size=1280,900', 'about:blank',
  ], 'chrome');
  const port = await until(() => {
    if (chrome.spawnError) throw chrome.spawnError;
    if (chrome.exitCode !== null) throw new Error(`Chrome exited: ${chrome.exitCode}`);
    try { return Number(readFileSync(join(profile, 'DevToolsActivePort'), 'utf8').split('\n')[0]); } catch { return false; }
  }, 'Chrome random CDP port');
  const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  const target = targets.find(t => t.type === 'page' && t.url === 'about:blank');
  assert.ok(target, 'owned blank Chrome page');
  send = await connect(target.webSocketDebuggerUrl);
  await send('Runtime.enable');
  await send('Page.enable');
  await send('Page.addScriptToEvaluateOnNewDocument', { source: instrument });
  await send('Emulation.setDeviceMetricsOverride', { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
  await send('Emulation.setFocusEmulationEnabled', { enabled: true });
  await send('Page.bringToFront');

  await load(`${ready.url}?backend=gl&ui_debug=1`);
  assert.equal((await snapshot()).calls.length, 0, 'normal load must not request camera');
  assert.ok(!(await uiTexts()).some(t => /开启摄像头|停止摄像头/.test(t.text)), 'camera window initially closed');
  await clickText(['视图']);
  await findText(['摄像头预览…']);
  await capture('menu-before-one-click');
  await clickText(['摄像头预览…']); // Exactly one command activation; no nested toolbar button fallback.
  await findText(['摄像头（Web 预览）', '摄像头预览']);
  await findText(['开启摄像头', '启用摄像头', '启动摄像头', '点击启用']);
  assert.equal((await snapshot()).calls.length, 0, 'opening window must not request device');
  evidence.stages.push({ stage: 'one-click-menu-open', snapshot: await snapshot(), screenshot: await capture('one-click-menu-open') });
  await clickText(['开启摄像头', '启用摄像头', '启动摄像头', '点击启用']);
  evidence.stages.push({ stage: 'menu-camera-live', snapshot: await live('menu activation'), screenshot: await capture('menu-camera-live') });
  await stop('menu-camera-stopped');

  // camera is deliberately FIRST, covering the historical '?camera=1' parser edge case.
  await load(`${ready.url}?camera=1&backend=gl&ui_debug=1`);
  await findText(['摄像头（Web 预览）', '摄像头预览']);
  evidence.stages.push({ stage: 'first-param-autostart-live', snapshot: await live('first-parameter autostart'), screenshot: await capture('first-param-autostart-live') });
  await stop('first-param-autostart-stopped');
  assert.deepEqual(evidence.runtimeExceptions, [], 'browser runtime exceptions');
  evidence.passed = true;
  console.log(`PASS fake-camera activation: one menu-command click, one getUserMedia request, UI stop ends every track; first-parameter camera=1 autostart and stop.\nEvidence: ${out}`);
}
async function cleanup() {
  if (cleaning) return cleaning;
  cleaning = (async () => {
    ws?.close();
    for (const proc of children.toReversed()) {
      if (!proc.pid) continue;
      // Each owned child is a new process group; include Chrome's renderer/device children.
      try { process.kill(-proc.pid, 'SIGTERM'); } catch (error) { if (error.code !== 'ESRCH') evidence.cleanupError = String(error); }
      await Promise.race([new Promise(resolve => {
        if (proc.exitCode !== null || proc.signalCode !== null) resolve(); else proc.once('exit', resolve);
      }), sleep(1500)]);
      try { process.kill(-proc.pid, 'SIGKILL'); } catch (error) { if (error.code !== 'ESRCH') evidence.cleanupError = String(error); }
    }
  })();
  return cleaning;
}
for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, () => {
  evidence.failure = `Interrupted by ${signal}`;
  process.exitCode = 1;
  cleanup().then(() => { writeFileSync(join(out, 'camera-activation-evidence.json'), JSON.stringify(evidence, null, 2)); process.exit(1); });
});
try { await main(); }
catch (error) {
  evidence.passed = false;
  evidence.failure = error.stack;
  if (send && ws?.readyState === WebSocket.OPEN) {
    try { evidence.failureSnapshot = await snapshot(); evidence.failureTexts = await uiTexts(); await capture('failure'); } catch {}
  }
  console.error(`FAIL camera activation: ${error.stack}\nEvidence: ${out}`);
  process.exitCode = 1;
} finally {
  await cleanup();
  writeFileSync(join(out, 'camera-activation-evidence.json'), JSON.stringify(evidence, null, 2));
  writeFileSync(join(out, 'README.md'), `# Camera activation browser probe\n\nCommand (after rebuilding web_client):\n\n\`\`\`sh\nCHROME=/usr/bin/google-chrome node tools/probe_web_camera_activation.mjs\n\`\`\`\n\nResult: ${evidence.passed ? 'PASS' : 'FAIL / interrupted'}. Fake device only; physical camera permission and hardware are not asserted.\n\nEvidence: camera-activation-evidence.json, named PNG screenshots, Chrome/server logs. Screenshots are captured evidence, not visually inspected. Profiles and artifacts are retained; owned child process groups are terminated.\n\nSHA-256:\n\n${Object.entries(evidence.hashes).map(([name, entry]) => `- ${name}: \`${entry.sha256}\``).join('\n')}\n`);
}
