#!/usr/bin/env node
// Real Chrome + wasm + same-origin HTTP + deterministic model tool loop.
// Run only after end-to-end phase authorization:
// CHROME=/path/to/chrome node tools/probe_web_agent.mjs
// Uses random loopback ports. Node 24 native WebSocket; no npm dependencies.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createInterface } from 'node:readline';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const out = resolve(process.env.OUT_DIR || join(root, 'target/tmp/agent-browser'));
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const children = [];
let ws;
let profile;
let control;
const evidence = { stages: [], errors: [] };

async function until(fn, label, timeout = 30000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) {
    const result = await fn();
    if (result) return result;
    await sleep(80);
  }
  throw new Error(`timeout: ${label}`);
}
function child(program, args, options = {}) {
  const proc = spawn(program, args, { cwd: root, stdio: ['pipe', 'pipe', 'pipe'], ...options });
  children.push(proc);
  proc.on('error', error => evidence.errors.push(String(error)));
  return proc;
}
function firstLine(proc) {
  return new Promise((resolve, reject) => {
    const lines = createInterface({ input: proc.stdout });
    lines.once('line', line => { lines.close(); resolve(line); });
    proc.once('exit', code => reject(new Error(`fixture exited before readiness: ${code}`)));
    proc.once('error', reject);
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
      const { resolve, reject, timer } = pending.get(message.id);
      clearTimeout(timer);
      pending.delete(message.id);
      message.error ? reject(new Error(JSON.stringify(message.error))) : resolve(message.result);
    }
    if (message.method === 'Runtime.exceptionThrown') evidence.errors.push(message.params);
  });
  return (method, params = {}) => new Promise((resolve, reject) => {
    const id = next++;
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`CDP timeout ${method}`)); }, 30000);
    pending.set(id, { resolve, reject, timer });
    ws.send(JSON.stringify({ id, method, params }));
  });
}
async function evaluate(send, expression) {
  const response = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
  if (response.exceptionDetails) throw new Error(JSON.stringify(response.exceptionDetails));
  return response.result?.value;
}
function scene(receipt) {
  assert.equal(receipt.ok, true, JSON.stringify(receipt));
  assert.equal(typeof receipt.revision, 'string');
  assert.ok(Array.isArray(receipt.result.objects), 'inspect must return actual objects');
  return receipt.result;
}
function ball(snapshot) {
  const matches = snapshot.objects.filter(object => object.name === 'AI球');
  assert.equal(matches.length, 1, 'exactly one model-created AI球');
  return matches[0];
}
function at(snapshot, expected) {
  const object = ball(snapshot);
  assert.deepEqual(object.position, expected, 'actual scene transform');
  assert.deepEqual(object.body_position, expected, 'actual physics body transform');
}

async function main() {
  mkdirSync(out, { recursive: true });
  const fixture = child(process.env.PYTHON || 'python3', ['tools/test_agent_fixture.py', '--serve']);
  fixture.stderr.on('data', data => writeFileSync(join(out, 'fixture.stderr.log'), data, { flag: 'a' }));
  const ready = JSON.parse(await firstLine(fixture));
  assert.match(ready.url, /^http:\/\/127\.0\.0\.1:\d+\//);
  assert.match(ready.control, /^http:\/\/127\.0\.0\.1:\d+$/);
  control = ready.control;
  const profileRoot = join(root, 'target/tmp/round220-ai-agent');
  mkdirSync(profileRoot, { recursive: true });
  profile = mkdtempSync(join(profileRoot, 'browser-profile-'));
  const chrome = child(process.env.CHROME || '/usr/bin/google-chrome', [
    '--headless=new', '--no-sandbox', '--disable-dev-shm-usage',
    '--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--remote-debugging-port=0',
    `--user-data-dir=${profile}`, '--window-size=1280,900', ready.url,
  ]);
  chrome.stderr.on('data', data => writeFileSync(join(out, 'chrome.stderr.log'), data, { flag: 'a' }));
  const port = await until(() => {
    try { return Number(readFileSync(join(profile, 'DevToolsActivePort'), 'utf8').split('\n')[0]); }
    catch { return false; }
  }, 'Chrome DevToolsActivePort');
  const target = await until(async () => {
    const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
    return targets.find(target => target.type === 'page' && target.url.startsWith(ready.url.split('?')[0]));
  }, 'editor page target');
  const send = await connect(target.webSocketDebuggerUrl);
  await send('Runtime.enable');
  await send('Page.enable');
  await send('Page.bringToFront');
  await until(() => evaluate(send, 'document.getElementById("overlay")?.classList.contains("hidden") === true'), 'first rendered editor frame', 60000);
  const browserResources = await evaluate(send, `(async () => {
    const paths = ['/', '/pkg/quench3d_web.js', '/api/agent/config'];
    return Promise.all(paths.map(async path => ({ path, text: await (await fetch(path)).text() })));
  })()`);
  for (const resource of browserResources) assert.equal(resource.text.includes('fixture-only-no-provider-key'), false, `server fixture credential leaked in ${resource.path}`);
  const exports = await evaluate(send, `(async () => {
    const mod = await import('/pkg/quench3d_web.js');
    window.__agentProbeModule = mod;
    return { enqueue: typeof mod.enqueue_editor_request, take: typeof mod.take_editor_result };
  })()`);
  assert.deepEqual(exports, { enqueue: 'function', take: 'function' }, 'built wasm must expose the real editor bridge');
  const request = async payload => {
    const id = await evaluate(send, `window.__agentProbeModule.enqueue_editor_request(${JSON.stringify(JSON.stringify(payload))})`);
    assert.equal(typeof id, 'number', 'bridge request id is u32');
    const raw = await until(() => evaluate(send, `window.__agentProbeModule.take_editor_result(${id}) ?? null`), `bridge ${payload.action}`);
    return JSON.parse(raw);
  };
  const status = () => request({ action: 'status' });
  const before = scene(await request({ action: 'inspect' }));
  assert.equal(before.objects.some(object => object.name === 'AI球'), false);
  assert.equal((await request({ action: 'start', prompt: 'fixture-main: 创建 AI球，移动到 [2,3,4]，检查并撤销重做。' })).ok, true);
  const done = await until(async () => {
    const value = await status();
    if (value.agent.status === 'error') throw new Error(JSON.stringify(value));
    return value.agent.status === 'done' && value;
  }, 'model tool loop completion', 60000);
  const final = scene(await request({ action: 'inspect' }));
  at(final, [2, 3, 4]);
  assert.equal(final.objects.length, before.objects.length + 1);
  assert.equal(final.paused, true);
  const report = await (await fetch(`${control}/fixture/status`)).json();
  assert.deepEqual(report.errors, []);
  assert.equal(report.completed, true);
  assert.equal(report.requests.length, 10, 'each tool result must return through real HTTP');
  // Independent actual scene/body inspections after transform, undo and redo.
  for (const index of [5, 9]) at(scene(report.requests[index].tool_results.at(-1)), [2, 3, 4]);
  at(scene(report.requests[7].tool_results.at(-1)), [0, 2, 0]);
  assert.equal(final.history.len, before.history.len + 2, 'spawn and transform each record exactly one edit');
  assert.equal(final.history.can_undo, true);
  assert.equal(final.history.can_redo, false);
  assert.equal(scene(report.requests[7].tool_results.at(-1)).history.can_redo, true, 'undo must make redo available');
  evidence.stages.push({ stage: 'model-loop', before, final, agent: done.agent, http: report });

  const cancelBefore = await request({ action: 'inspect' });
  assert.equal((await request({ action: 'start', prompt: `fixture-cancel:${JSON.stringify({ revision: cancelBefore.revision })}` })).ok, true);
  await until(async () => (await (await fetch(`${control}/fixture/status`)).json()).delayed, 'delayed model HTTP request');
  const waiting = await status();
  await until(async () => (await status()).frame > waiting.frame + 4, 'frames progress while model HTTP is waiting');
  assert.equal((await request({ action: 'cancel' })).ok, true);
  await fetch(`${control}/fixture/release`, { method: 'POST' });
  await until(async () => (await (await fetch(`${control}/fixture/status`)).json()).late_sent, 'upstream completed late response attempt');
  const cancelled = await until(async () => {
    const current = await status();
    return current.agent.status === 'cancelled' && current.frame > waiting.frame + 12 && current;
  }, 'cancellation remains effective after late response');
  // Allow another rendered interval after upstream attempted the late response.
  await until(async () => (await status()).frame > cancelled.frame + 12, 'late response drain frames');
  const cancelAfter = await request({ action: 'inspect' });
  assert.deepEqual(cancelAfter, cancelBefore, 'late model response must not mutate scene/revision/history');
  assert.equal((await status()).agent.status, 'cancelled');
  const cancelReport = await (await fetch(`${control}/fixture/status`)).json();
  assert.deepEqual(cancelReport.errors, []);
  assert.equal(cancelReport.cancel_requests, 1);
  evidence.stages.push({ stage: 'nonblocking-cancel', waiting, cancelled, cancelBefore, cancelAfter });
  const screenshot = await send('Page.captureScreenshot', { format: 'png' });
  writeFileSync(join(out, 'agent-final.png'), Buffer.from(screenshot.data, 'base64'));
  assert.deepEqual(evidence.errors, [], 'browser runtime exceptions');
  console.log('PASS probe-web-agent: real model HTTP tool loop, body/history, frame progress and cancelled HTTP result causes no edits');
}

try { await main(); }
catch (error) { evidence.failure = error.stack; console.error(`FAIL probe-web-agent: ${error.stack}`); process.exitCode = 1; }
finally {
  mkdirSync(out, { recursive: true });
  writeFileSync(join(out, 'agent-evidence.json'), JSON.stringify(evidence, null, 2));
  if (control) { try { await fetch(`${control}/fixture/release`, { method: 'POST' }); } catch {} }
  ws?.close();
  for (const proc of children.reverse()) {
    if (proc.exitCode === null && proc.signalCode === null) {
      proc.stdin.end();
      proc.kill('SIGTERM');
      await Promise.race([new Promise(resolve => proc.once('exit', resolve)), sleep(3000)]);
      if (proc.exitCode === null && proc.signalCode === null) {
        const exited = new Promise(resolve => proc.once('exit', resolve));
        proc.kill('SIGKILL');
        await exited;
      }
    }
  }
  // Retain the unique workspace profile and evidence for inspection.
  if (profile) console.log(`Chrome profile retained: ${profile}`);
}
