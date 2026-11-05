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
  await until(() => evaluate(send, `location.origin === ${JSON.stringify(new URL(ready.url).origin)} && document.readyState !== 'loading'`), 'same-origin document ready before local configuration');
  const clientConfig = {
    version: 1,
    providers: [{ id: 'fixture', name: 'Local test proxy', base_url: `${control}/v1`, api_key: 'fixture-client-only-key' }],
    models: [{ id: 'fixture-model', name: 'Fixture model', provider_id: 'fixture', model: 'loopback-fixture', temperature: 0.2, max_tokens: 2048, timeout_secs: 25 }],
    selected_model: 'fixture-model',
  };
  await evaluate(send, `localStorage.setItem('quench3d.ai.config.v1', ${JSON.stringify(JSON.stringify(clientConfig))})`);
  await send('Page.reload');
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
  assert.equal((await request({ action: 'start', prompt: 'fixture-main' })).ok, true);
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

  const followupPrompt = 'fixture-followup: AI球刚才移动到了哪里？沿用之前的场景工具结果回答。';
  assert.equal((await request({ action: 'start', prompt: followupPrompt })).ok, true);
  const followed = await until(async () => {
    const value = await status();
    if (value.agent.status === 'error') throw new Error(JSON.stringify(value));
    return value.agent.status === 'done' && value.agent.messages.some(m =>
      m.role === 'assistant' && String(m.content).includes('fixture-followup-ok')) && value;
  }, 'continued conversation HTTP context', 60000);
  const followupReport = await (await fetch(`${control}/fixture/status`)).json();
  assert.deepEqual(followupReport.errors, []);
  assert.equal(followupReport.followup_requests.length, 1);
  assert.equal(followupReport.followup_requests[0].tool_results, 9);
  assert.ok(followed.agent.messages.some(m => m.role === 'user' && String(m.content).includes('fixture-main')));
  assert.ok(followed.agent.messages.some(m => m.role === 'user' && m.content === followupPrompt));
  assert.equal(followed.agent.messages.filter(m => m.role === 'tool').length, 9);
  assert.deepEqual(scene(await request({ action: 'inspect' })), final, 'a text followup must not edit the scene');
  evidence.stages.push({ stage: 'ongoing-conversation', agent: followed.agent, http: followupReport.followup_requests });

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
  const historyBefore = await until(async () => {
    const history = await evaluate(send, `JSON.parse(localStorage.getItem('quench3d.ai.history.v1') || 'null')`);
    const active = history?.conversations?.find(c => c.id === history.active_id);
    return active?.messages?.some(m => String(m.content).includes('fixture-followup-ok')) && history;
  }, 'conversation history saved locally');
  const activeBefore = historyBefore.conversations.find(c => c.id === historyBefore.active_id);
  assert.equal(typeof activeBefore.title, 'string');
  assert.ok(activeBefore.title.length > 0);
  const historyText = JSON.stringify(historyBefore);
  assert.equal(historyText.includes('fixture-client-only-key'), false, 'history must never contain provider credentials');
  assert.equal(historyText.includes('"api_key"'), false, 'history schema has no connection credentials');
  const transcriptBefore = (await status()).agent.messages;
  assert.deepEqual(activeBefore.messages, transcriptBefore, 'stored active transcript matches the running conversation');
  await send('Page.reload');
  await until(() => evaluate(send, 'document.getElementById("overlay")?.classList.contains("hidden") === true'), 'reloaded editor frame', 60000);
  // A new WASM instance requires re-importing the bridge after navigation.
  await evaluate(send, `(async () => { window.__agentProbeModule = await import('/pkg/quench3d_web.js'); })()`);
  const restored = await status();
  assert.deepEqual(restored.agent.messages, transcriptBefore, 'reload restores complete messages including tool receipts and followup');
  assert.ok(!['requesting', 'tools'].includes(restored.agent.status), 'history restoration never resumes execution');
  await until(async () => (await status()).frame > restored.frame + 12, 'restored history remains idle for rendered frames');
  const reportAfterReload = await (await fetch(`${control}/fixture/status`)).json();
  assert.equal(reportAfterReload.requests.length, cancelReport.requests.length, 'reload must not restart main model loop');
  assert.equal(reportAfterReload.followup_requests.length, 1, 'reload must not resend the followup');
  assert.equal(reportAfterReload.cancel_requests, 1, 'reload must not resume cancelled request');
  assert.deepEqual(reportAfterReload.errors, []);
  const historyAfter = await evaluate(send, `JSON.parse(localStorage.getItem('quench3d.ai.history.v1'))`);
  assert.equal(historyAfter.active_id, historyBefore.active_id, 'active conversation selection survives reload');
  evidence.stages.push({ stage: 'history-reload', history: historyAfter, restored: restored.agent, upstream: reportAfterReload });

  const controls = () => evaluate(send, `JSON.parse(document.body?.getAttribute('data-quench3d-model-controls') || '[]')`);
  const rendered = async () => {
    const frame = (await status()).frame;
    await until(async () => (await status()).frame > frame + 2, 'pointer rendering frames');
  };
  const click = async text => {
    const item = await until(async () => (await controls()).find(c => c.text === text), `visible history control ${text}`);
    const [x0, y0, x1, y1] = item.rect;
    const x = (x0 + x1) / 2, y = (y0 + y1) / 2;
    const viewport = await evaluate(send, '({w:innerWidth,h:innerHeight})');
    assert.ok(x > 0 && y > 0 && x < viewport.w && y < viewport.h, `offscreen history control ${text}`);
    await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y });
    await rendered();
    await Promise.all([
      send('Input.dispatchMouseEvent', { type: 'mousePressed', button: 'left', buttons: 1, clickCount: 1, x, y }),
      send('Input.dispatchMouseEvent', { type: 'mouseReleased', button: 'left', buttons: 0, clickCount: 1, x, y }),
    ]);
    await rendered();
  };
  await click('新建会话');
  const newChat = await until(async () => {
    const h = await evaluate(send, `JSON.parse(localStorage.getItem('quench3d.ai.history.v1'))`);
    return h.active_id !== historyBefore.active_id && h;
  }, 'new conversation selected and persisted');
  assert.equal(newChat.conversations.length, historyAfter.conversations.length + 1);
  assert.equal((await status()).agent.messages.some(m => m.role === 'user'), false, 'new conversation has no previous prompts');
  await click('新会话'); // The actual history ComboBox's selected title.
  await click('fixture-main');
  const switched = await until(async () => {
    const h = await evaluate(send, `JSON.parse(localStorage.getItem('quench3d.ai.history.v1'))`);
    return h.active_id === historyBefore.active_id && (await status());
  }, 'switch back to previous conversation through history UI');
  assert.deepEqual(switched.agent.messages, transcriptBefore, 'switching conversations restores the complete original transcript');
  const finalReport = await (await fetch(`${control}/fixture/status`)).json();
  assert.equal(finalReport.requests.length, cancelReport.requests.length);
  assert.equal(finalReport.followup_requests.length, 1);
  assert.equal(finalReport.cancel_requests, 1);
  evidence.stages.push({ stage: 'new-and-switch-conversations-ui', newChat, switched: switched.agent });

  // Continue the restored conversation past the former 32-request ceiling.
  // Large assistant progress text forces request compaction while the persisted
  // transcript must retain every assistant/tool receipt pair.
  assert.equal((await request({ action: 'start', prompt: 'fixture-long: 完成40次检查，保持当前目标和最新工具回执。' })).ok, true);
  const longDone = await until(async () => {
    const current = await status();
    if (current.agent.status === 'error') throw new Error(JSON.stringify(current));
    return current.agent.status === 'done' && current.agent.messages.some(message =>
      message.role === 'assistant' && String(message.content).includes('fixture-long-done')) && current;
  }, '40-tool long-context conversation completion', 120000);
  const longReport = await (await fetch(`${control}/fixture/status`)).json();
  assert.deepEqual(longReport.errors, []);
  assert.equal(longReport.long_completed, true);
  assert.equal(longReport.long_requests.length, 41, '40 tools require 41 actual model requests');
  assert.deepEqual(longReport.long_requests.map(item => item.round), Array.from({ length: 41 }, (_, i) => i));
  assert.ok(longReport.long_requests.length > 32, 'long conversation must pass the former request ceiling');
  assert.equal(longReport.seen_compaction, true, 'requests must contain additional summary system context');
  const compressed = longReport.long_requests.filter(item => item.system_count > 1);
  assert.ok(compressed.length > 0);
  assert.ok(compressed.every(item => item.message_bytes < 48000), 'compressed message contexts stay below trigger budget');
  assert.ok(longReport.long_requests.at(-1).request_bytes < 48000, 'final full request including tools is bounded');
  const longHistory = await until(async () => {
    const history = await evaluate(send, `JSON.parse(localStorage.getItem('quench3d.ai.history.v1'))`);
    const active = history.conversations.find(c => c.id === history.active_id);
    return active.messages.some(m => String(m.content).includes('fixture-long-done')) && history;
  }, 'complete long conversation saved');
  const longActive = longHistory.conversations.find(c => c.id === longHistory.active_id);
  assert.ok(longActive.messages.length > 80, 'history keeps the full transcript rather than compact request context');
  assert.equal(longActive.messages.filter(m => m.role === 'tool' && /^long_\d+$/.test(m.tool_call_id)).length, 40);
  assert.deepEqual(longActive.messages, longDone.agent.messages);
  evidence.stages.push({ stage: 'long-context-compaction', agent: longDone.agent,
    http: longReport.long_requests, full_message_count: longActive.messages.length });

  // Exercise the advertised orbit tools through actual model HTTP, not direct
  // scene mutation. Only inspect while playing because revisions change per step.
  assert.equal((await request({ action: 'start', prompt: 'fixture-orbit: 创建静态太阳和绕其公转的地球，显示两个物体的中文名称标签，然后播放。' })).ok, true);
  const orbitDone = await until(async () => {
    const current = await status();
    if (current.agent.status === 'error') throw new Error(JSON.stringify(current));
    return current.agent.status === 'done' && current.agent.messages.some(m =>
      m.role === 'assistant' && m.content === 'fixture-orbit-done') && current;
  }, 'actual AI orbit setup', 60000);
  const labelCalls = orbitDone.agent.messages.flatMap(message => message.tool_calls ?? [])
    .filter(call => /^orbit_\d+$/.test(call.id) && call.function.name === 'set_object_label');
  assert.equal(labelCalls.length, 2, 'AI executes both label tools');
  assert.deepEqual(labelCalls.map(call => JSON.parse(call.function.arguments).name), ['太阳', '地球']);
  for (const call of labelCalls) {
    const args = JSON.parse(call.function.arguments);
    assert.equal(args.visible, true);
    assert.equal(typeof args.expected_revision, 'string');
    const receipts = orbitDone.agent.messages.filter(message => message.role === 'tool' && message.tool_call_id === call.id);
    assert.equal(receipts.length, 1, `one label receipt for ${args.name}`);
    assert.equal(JSON.parse(receipts[0].content).ok, true, `label tool succeeded for ${args.name}`);
  }
  const orbitObject = (snapshot, name, radius) => {
    const matches = snapshot.objects.filter(object => object.name === name);
    assert.equal(matches.length, 1, `unique ${name}`);
    const object = matches[0];
    assert.equal(object.shape.type, 'sphere');
    assert.equal(object.label_visible, true, `${name} Chinese name label remains enabled`);
    assert.ok(Math.abs(object.shape.radius - radius) < 1e-5, `${name} sphere radius`);
    assert.ok(Array.isArray(object.body_position), `${name} has physical body`);
    const visualLag = Math.hypot(...object.position.map((value, index) => value - object.body_position[index]));
    // Live rendering interpolates adjacent 60 Hz physical poses. At radius 3
    // and period 2 the maximum one-step chord is ~0.157 m.
    assert.ok(visualLag < (snapshot.paused ? 1e-4 : 0.17), `${name} interpolation stays within one physical step: ${visualLag}`);
    return object;
  };
  const checkOrbit = snapshot => {
    const sun = orbitObject(snapshot, '太阳', 0.8);
    const planet = orbitObject(snapshot, '地球', 0.2);
    sun.position.forEach((value, index) => assert.ok(Math.abs(value - [0, 5, 0][index]) < 1e-4, 'sun stays fixed'));
    assert.ok(Math.abs(Math.hypot(planet.body_position[0], planet.body_position[2]) - 3) < 1e-3, 'physical orbital radius stays 3');
    assert.ok(Math.abs(Math.hypot(planet.position[0], planet.position[2]) - 3) < 0.002, 'interpolated orbital radius stays near 3');
    assert.ok(Math.abs(planet.position[1] - 5) < 1e-4, 'xz orbit stays at height 5');
    return { sun, planet };
  };
  const orbitFirst = scene(await request({ action: 'inspect' }));
  assert.equal(orbitFirst.paused, false);
  const firstOrbitObjects = checkOrbit(orbitFirst);
  const moved = await until(async () => {
    const snapshot = scene(await request({ action: 'inspect' }));
    const objects = checkOrbit(snapshot);
    const distance = Math.hypot(...objects.planet.position.map((value, i) => value - firstOrbitObjects.planet.position[i]));
    return distance > 0.3 && { snapshot, objects, distance };
  }, 'orbit advances with actual simulation time', 30000);
  assert.equal((await request({ action: 'start', prompt: 'fixture-orbit-stop: 暂停公转。' })).ok, true);
  await until(async () => {
    const current = await status();
    if (current.agent.status === 'error') throw new Error(JSON.stringify(current));
    return current.agent.status === 'done' && current.agent.messages.some(m =>
      m.role === 'assistant' && m.content === 'fixture-orbit-stopped');
  }, 'AI pauses orbit');
  const pausedOrbit = scene(await request({ action: 'inspect' }));
  assert.equal(pausedOrbit.paused, true);
  const pausedObjects = checkOrbit(pausedOrbit);
  const pauseFrame = (await status()).frame;
  await until(async () => (await status()).frame > pauseFrame + 20, 'paused orbit rendered frames');
  const stillOrbit = scene(await request({ action: 'inspect' }));
  const stillObjects = checkOrbit(stillOrbit);
  assert.deepEqual(stillObjects.planet.position, pausedObjects.planet.position, 'pause stops orbital motion');
  assert.deepEqual(stillObjects.sun.position, pausedObjects.sun.position, 'paused sun remains fixed');
  const orbitReport = await (await fetch(`${control}/fixture/status`)).json();
  assert.deepEqual(orbitReport.errors, []);
  assert.deepEqual(orbitReport.orbit_requests.map(item => item.round), [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
  assert.equal(orbitReport.orbit_stop_requests.length, 2);
  assert.equal(orbitReport.long_requests.length, 41, 'orbit stage does not rerun the long task');
  evidence.stages.push({ stage: 'actual-ai-orbit', agent: orbitDone.agent, first: orbitFirst,
    moved, paused: pausedOrbit, still: stillOrbit, label_calls: labelCalls, http: orbitReport.orbit_requests });

  const screenshot = await send('Page.captureScreenshot', { format: 'png' });
  writeFileSync(join(out, 'agent-final.png'), Buffer.from(screenshot.data, 'base64'));
  assert.deepEqual(evidence.errors, [], 'browser runtime exceptions');
  console.log('PASS probe-web-agent: real HTTP tools with omitted content, followup context, cancellation, persisted transcript reload without re-execution');
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
