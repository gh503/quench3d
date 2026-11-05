#!/usr/bin/env node
// Real Chrome/egui pointer clicks; requires rebuilt Web and read-only model-controls metadata.
// CHROME=/usr/bin/google-chrome node tools/probe_web_models.mjs
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createInterface } from 'node:readline';
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
if (process.argv.includes('--help')) {
  console.log('After Web rebuild: CHROME=/usr/bin/google-chrome node tools/probe_web_models.mjs\nRequires Node 22+, Python3, read-only data-quench3d-model-controls metadata. Retains evidence and Chrome profile.');
  process.exit(0);
}
const base = resolve(process.env.OUT_DIR || join(root, 'target/tmp/web-models'));
mkdirSync(base, { recursive: true });
const out = mkdtempSync(join(base, 'discovery-'));
const profile = join(out, 'chrome-profile');
mkdirSync(profile);
const evidence = { passed: false, out, clicks: [], stages: [], runtimeErrors: [], hashes: {}, typingRegressionTested: false };
const children = [];
let ws, send, control;
const sleep = ms => new Promise(r => setTimeout(r, ms));
async function until(fn, label, timeout = 30000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) { const result = await fn(); if (result) return result; await sleep(120); }
  throw new Error(`timeout: ${label}`);
}
function child(program, args, name) {
  const proc = spawn(program, args, { cwd: root, detached: true, stdio: ['pipe', 'pipe', 'pipe'] });
  children.push(proc);
  proc.on('error', error => { proc.launchError = error; });
  for (const [stream, suffix] of [[proc.stdout, 'stdout'], [proc.stderr, 'stderr']]) {
    stream.on('data', data => writeFileSync(join(out, `${name}.${suffix}.log`), data, { flag: 'a' }));
  }
  return proc;
}
function firstLine(proc) {
  return new Promise((resolve, reject) => {
    const lines = createInterface({ input: proc.stdout });
    const timer = setTimeout(() => { lines.close(); reject(new Error('fixture readiness timeout')); }, 15000);
    lines.once('line', line => { clearTimeout(timer); lines.close(); resolve(line); });
    proc.once('error', error => { clearTimeout(timer); reject(error); });
    proc.once('exit', code => { clearTimeout(timer); reject(new Error(`fixture exited ${code}`)); });
  });
}
async function connect(url) {
  ws = new WebSocket(url);
  await new Promise((resolve, reject) => { ws.addEventListener('open', resolve, { once: true }); ws.addEventListener('error', reject, { once: true }); });
  let next = 0;
  const pending = new Map();
  ws.addEventListener('message', event => {
    const message = JSON.parse(event.data);
    if (pending.has(message.id)) {
      const p = pending.get(message.id); pending.delete(message.id); clearTimeout(p.timer);
      message.error ? p.reject(new Error(JSON.stringify(message.error))) : p.resolve(message.result);
    }
    if (message.method === 'Runtime.exceptionThrown') evidence.runtimeErrors.push(message.params);
  });
  send = (method, params = {}) => new Promise((resolve, reject) => {
    const id = ++next;
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`CDP timeout ${method}`)); }, 30000);
    pending.set(id, { resolve, reject, timer }); ws.send(JSON.stringify({ id, method, params }));
  });
}
async function evaluate(expression) {
  const response = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
  if (response.exceptionDetails) throw new Error(JSON.stringify(response.exceptionDetails));
  return response.result?.value;
}
const controls = () => evaluate(`JSON.parse(document.body?.getAttribute('data-quench3d-model-controls') || '[]')`);
const geometry = () => evaluate(`(() => ({ viewport: {width:innerWidth,height:innerHeight,dpr:devicePixelRatio}, visual: {width:visualViewport.width,height:visualViewport.height,scale:visualViewport.scale,offsetLeft:visualViewport.offsetLeft,offsetTop:visualViewport.offsetTop}, canvases: [...document.querySelectorAll('canvas')].map(c => { const r=c.getBoundingClientRect(); return {id:c.id,width:c.width,height:c.height,rect:{x:r.x,y:r.y,width:r.width,height:r.height},css:{width:getComputedStyle(c).width,height:getComputedStyle(c).height,transform:getComputedStyle(c).transform}}; }), pointerEvents:window.__modelProbePointerEvents || [] }))()`);
async function find(names, timeout = 30000) {
  return until(async () => (await controls()).find(item => names.includes(item.text)), `visible control ${names.join('/')}`, timeout);
}
async function renderedFrames(count = 2) {
  const start = await evaluate('window.__modelProbeFrames || 0');
  await until(() => evaluate(`(window.__modelProbeFrames || 0) >= ${start + count}`), `${count} actual egui frames`, 20000);
}
async function click(names) {
  await find(names);
  // Let the egui window finish sizing before measuring a pointer target.
  await sleep(250);
  const item = await find(names);
  assert.ok(Array.isArray(item.rect) && item.rect.length === 4 && item.rect.every(Number.isFinite), JSON.stringify(item));
  const [x0, y0, x1, y1] = item.rect;
  const x = (x0 + x1) / 2, y = (y0 + y1) / 2;
  const viewport = await evaluate('({w:innerWidth,h:innerHeight})');
  assert.ok(x1 > x0 && y1 > y0 && x > 0 && y > 0 && x < viewport.w && y < viewport.h, `offscreen ${JSON.stringify(item)}`);
  evidence.clicks.push({ text: item.text, rect: item.rect, x, y, geometry: await geometry() });
  await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y }); await renderedFrames();
  await Promise.all([
    send('Input.dispatchMouseEvent', { type: 'mousePressed', button: 'left', buttons: 1, clickCount: 1, x, y }),
    send('Input.dispatchMouseEvent', { type: 'mouseReleased', button: 'left', buttons: 0, clickCount: 1, x, y }),
  ]);
  await renderedFrames();
  evidence.clicks.at(-1).afterControls = await controls();
}
async function capture(name) {
  const shot = await send('Page.captureScreenshot', { format: 'png' });
  writeFileSync(join(out, `${name}.png`), Buffer.from(shot.data, 'base64'));
}
async function status() { return (await fetch(`${control}/fixture/status`)).json(); }
const saved = () => evaluate(`JSON.parse(localStorage.getItem('quench3d.ai.config.v1'))`);
function redacted(config) { return { ...config, providers: config.providers.map(({ api_key, ...p }) => ({ ...p, credentialPresent: !!api_key })) }; }
async function main() {
  for (const path of ['web/pkg/quench3d_web_bg.wasm', 'web/pkg/quench3d_web.js', 'tools/probe_web_models.mjs']) {
    evidence.hashes[path] = createHash('sha256').update(readFileSync(join(root, path))).digest('hex');
  }
  const fixture = child(process.env.PYTHON || 'python3', ['tools/test_agent_fixture.py', '--serve'], 'fixture');
  const ready = JSON.parse(await firstLine(fixture));
  control = ready.control;
  assert.match(control, /^http:\/\/127\.0\.0\.1:\d+$/);
  const url = new URL(ready.url); url.searchParams.set('backend', 'gl'); url.searchParams.set('ui_debug', '1');
  const chrome = child(process.env.CHROME || '/usr/bin/google-chrome', [
    '--headless=new', '--no-sandbox', '--disable-dev-shm-usage', '--use-angle=swiftshader', '--enable-unsafe-swiftshader',
    '--remote-debugging-port=0', `--user-data-dir=${profile}`, '--window-size=1600,1200', 'about:blank',
  ], 'chrome');
  const port = await until(() => { if (chrome.launchError) throw chrome.launchError; try { return Number(readFileSync(join(profile, 'DevToolsActivePort'), 'utf8').split('\n')[0]); } catch { return false; } }, 'Chrome CDP port');
  const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  await connect(targets.find(t => t.type === 'page').webSocketDebuggerUrl);
  await send('Page.enable'); await send('Runtime.enable'); await send('Page.bringToFront');
  const config = { version: 1, providers: [{ id: 'discovery', name: 'Discovery fixture', base_url: `${control}/v1`, api_key: 'fixture-client-only-key' }], models: [], selected_model: null };
  // Seed before wasm initializes; then remove initializer so reload genuinely exercises persistence.
  const script = await send('Page.addScriptToEvaluateOnNewDocument', { source: `localStorage.setItem('quench3d.ai.config.v1', ${JSON.stringify(JSON.stringify(config))}); window.__modelProbeFrames=0; new MutationObserver(ms=>{window.__modelProbeFrames+=ms.filter(m=>m.attributeName==='data-quench3d-model-controls').length;}).observe(document,{subtree:true,attributes:true,attributeFilter:['data-quench3d-model-controls']}); window.__modelProbePointerEvents=[]; for (const type of ['pointermove','pointerdown','pointerup','mousedown','mouseup','click']) addEventListener(type,e=>{window.__modelProbePointerEvents.push({type,x:e.clientX,y:e.clientY,offsetX:e.offsetX,offsetY:e.offsetY,target:e.target.id,buttons:e.buttons,trusted:e.isTrusted}); if(window.__modelProbePointerEvents.length>80)window.__modelProbePointerEvents.shift();},true);` });
  await send('Page.navigate', { url: url.href });
  await until(() => evaluate(`document.getElementById('overlay')?.classList.contains('hidden') === true && location.origin === ${JSON.stringify(url.origin)}`), 'first rendered editor frame', 90000);
  await send('Page.removeScriptToEvaluateOnNewDocument', { identifier: script.identifier });
  await find(['AI 助手']);
  const sidebarControl = await find(['本地模型配置']); // Never click toolbar AI to open it.
  const viewportWidth = await evaluate('innerWidth');
  assert.ok(sidebarControl.rect[0] > viewportWidth / 2, 'AI configuration must render in the default right sidebar');
  evidence.stages.push({ stage: 'default-ai-sidebar', config: redacted(await saved()) });
  assert.deepEqual((await saved()).models, []);
  assert.equal((await saved()).selected_model, null);
  assert.equal((await status()).models_requests.length, 0);
  await click(['本地模型配置']);
  await find(['服务商：Discovery fixture']);
  if (!(await controls()).some(item => item.text === '获取可用模型')) await click(['服务商：Discovery fixture']);
  await click(['获取可用模型']);
  await until(async () => (await status()).models_requests.length === 1, 'actual upstream GET /v1/models');
  const requested = await status();
  assert.deepEqual(requested.models_requests, [{ path: '/v1/models', authenticated: true }]);
  await find(['model-alpha']); await find(['model-beta']);
  await capture('discovered-models');
  await click(['model-alpha']); await click(['model-beta']);
  await click(['添加所选', '添加选中模型']);
  // No explicit save: this verifies automatic persistence before reloading.
  const persisted = await until(async () => { const c = await saved(); return c.models.length === 2 && c; }, 'UI saves discovered models');
  assert.deepEqual(persisted.models.map(m => m.model).sort(), ['model-alpha', 'model-beta']);
  assert.ok(persisted.models.every(m => m.provider_id === 'discovery'));
  assert.equal(new Set(persisted.models.map(m => m.id)).size, 2);
  assert.ok(persisted.models.some(m => m.id === persisted.selected_model), 'added model must become selected');
  evidence.stages.push({ stage: 'models-added-and-saved', config: redacted(persisted), upstream: requested.models_requests });
  await capture('saved-models');
  await send('Page.reload');
  await until(() => evaluate(`document.getElementById('overlay')?.classList.contains('hidden') === true`), 'editor after reload', 90000);
  await find(['本地模型配置']);
  assert.deepEqual(await saved(), persisted, 'reload must preserve user model/provider binding and selection');
  assert.equal((await status()).models_requests.length, 1, 'reload must not fetch models implicitly');
  evidence.stages.push({ stage: 'reload-persistence', config: redacted(await saved()) });
  await capture('reloaded');
  assert.deepEqual(evidence.runtimeErrors, []);
  evidence.passed = true;
  console.log(`PASS Web models: real UI discovery, authenticated GET, selected models provider binding, save and reload.\nEvidence: ${out}`);
}
async function cleanup() {
  try { ws?.close(); } catch {}
  for (const proc of children.reverse()) { try { process.kill(-proc.pid, 'SIGTERM'); } catch {} }
}
try { await main(); }
catch (error) { evidence.failure = String(error.stack || error); console.error(`FAIL Web models: ${evidence.failure}\nEvidence: ${out}`); process.exitCode = 1; try { evidence.failureControls = await controls(); evidence.failureGeometry = await geometry(); await capture('failure'); } catch {} }
finally {
  writeFileSync(join(out, 'models-evidence.json'), JSON.stringify(evidence, null, 2));
  writeFileSync(join(out, 'README.md'), `# Web model discovery probe\n\nCommand: \`CHROME=/usr/bin/google-chrome node tools/probe_web_models.mjs\`\n\nPassed: ${evidence.passed}\n\nReal Chrome CDP pointer clicks through egui; fixture GET observes authentication without retaining key text. No screenshots visually inspected. Provider typing regression not tested (no field metadata available).\n`);
  await cleanup();
}
