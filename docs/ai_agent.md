# Live editor AI

The desktop and Web editor share an AI panel, a conversation state machine and the same live command dispatcher. The toolbar entry is “AI 助手”. A goal produces model requests, editor tool calls, actual tool receipts, and follow-up requests until completion, cancellation or error. Normal edits operate on the currently visible scene. HTTP work does not block the editor frame thread.

## Connect

Desktop: configure an OpenAI-compatible API base URL, model and optional session key in the panel, or set `QUENCH3D_LLM_BASE_URL`, `QUENCH3D_LLM_MODEL`, `QUENCH3D_LLM_API_KEY` before launching. Credentials are not persisted by the panel.

Web: run `python3 tools/agent_server.py` from the repository or extracted package root. The server serves the editor and `/api/agent/chat` on `http://127.0.0.1:8765`. Configure the same environment variables on the server. Loopback model endpoints may omit the key; cloud endpoints require it. The proxy supports an API base path or a complete `/chat/completions` endpoint. A static server alone does not provide the model endpoint. The development proxy binds only to localhost.

Example with a local OpenAI-compatible provider:

```bash
QUENCH3D_LLM_BASE_URL=http://localhost:11434/v1 \
QUENCH3D_LLM_MODEL=qwen2.5:7b \
python3 tools/agent_server.py
```

A useful first goal is: “创建半径 0.4 米的红球，放在 [0,3,0]，聚焦它，运行仿真。” Tool support by the chosen model is required.

## Current tools and contracts

`inspect_scene`, `spawn_primitive`, `set_transform`, `set_shape`, `set_color`, `delete_object`, `select_object`, `focus_object`, `set_playback`, `step`, `undo`, `redo`.

Targets use unique exact object names. Ambiguous or absent names fail explicitly. Inspect returns the actual scene, physical body positions, shape capabilities and history availability. Mutations return the new revision and actual success/error result. All tools except inspection and playback require `expected_revision`; pause before inspecting and editing. A stale revision triggers a fresh inspection rather than a guessed retry. Revisions are state fingerprints, not access tokens or monotonically increasing sequence numbers.

Primitive dimensions are physical dimensions in meters, limited to 0.0001 through 10000. Position and rotation edits preserve visual shape dimensions and unrelated body handles and velocities. Model members, generated objects, compound bodies and hierarchies need dedicated structure tools and are currently rejected for editing. Operations requiring scene history reject content that the existing scene serializer cannot represent. Undo/redo restores scene edits, not simulation velocity or contact caches.

Tools execute at most once per frame. A response may contain at most 16 tool calls; a goal may make at most 32 model requests. Malformed arguments, duplicate call IDs, invalid response roles and incomplete protocol responses fail explicitly. Tool receipts retain the original tool-call ID. Standard assistant messages with `content: null` and tool calls are preserved through subsequent requests.

## Stop and replace goals

Stop clears pending tool calls and increments the session generation. Late replies from earlier generations cannot execute tools. Web additionally aborts the fetch. Desktop uses bounded background HTTP workers; a cancelled underlying HTTP request can remain alive until its timeout, but its result cannot edit the scene. Loading a different scene invalidates the previous goal.

The browser automation exports `enqueue_editor_request` and `take_editor_result` address this same editor instance. Supported actions are `start`, `cancel`, `status`, `inspect` and `command`. IDs are JavaScript numbers (`u32`). At most 32 pending or unread requests may exist; callers must retrieve results to release capacity. Accepted results are retained until retrieved. This bridge does not own a separate simulation world.

## Verification

The implementation was first tested against placeholders: CPU conversation tests failed to produce tool receipts, three real GPU command tests failed on unimplemented inspection, and Chrome loaded the old editor but failed because its live bridge exports were absent.

Desktop tests use a real local HTTP model fixture, background `LlmClient`, live command dispatcher and GPU engine. They check scene/body positions, actual revisions and receipts, and advancing frames during HTTP work. The independent GPU command tests cover edits, undo/redo, invalid arguments, extreme sizes, stale revisions and ambiguous names.

The browser probe uses real Chrome, rebuilt WebAssembly, the same-origin HTTP proxy and a deterministic model fixture. It checks creation, transform, inspection, undo and redo through actual model HTTP requests. Cancellation checks frame progress and unchanged scene/history after an upstream late response attempt. Browser cancellation demonstrates active fetch abort; generation rejection is independently exercised by the CPU session test. The fixture's `late_sent` flag records an upstream send attempt, not proof of browser delivery.

`tools/gate.sh` includes proxy HTTP tests, fixture tests and, when Chrome is available, the browser AI probe after rebuilding the Web artifacts. Standalone probe: `node tools/probe_web_agent.mjs` with Node 24 and Chrome. Its JSON evidence and screenshot are retained under the configured `OUT_DIR`.

The first integrated run passed 1327 workspace tests, default-scene bit compatibility (1240 states / 931616648 payload bytes), the fixed native cost gates, Web build (14540277 bytes), packaging, artifact checks, the browser AI probe and the browser benchmark. The AI probe recorded 10 actual model HTTP rounds, scene and body position [2,3,4], history length 1→3 with verified undo/redo, advancing frames during the delayed request, identical complete inspection receipts after cancellation, and zero browser errors. That full-gate run still exited 1 because of Clippy findings, so it is not a full-gate pass. Subsequent lint, resource-lifetime and packaging fixes require final revalidation. Evidence: `target/tmp/round220-ai-agent/gate/`.

Final revalidation completed with exit 0: 1328 workspace tests passed, both native and Wasm Clippy passed with warnings denied, documentation/license/proxy gates passed, default bits and fixed cost gates passed, WebAssembly rebuilt to 14541021 bytes, the 780-file package passed content/manifest/checksum checks, and both real Chrome AI and browser benchmark gates passed. The package now contains its Web entry point, same-origin proxy and this guide. Four actual GPU command tests include repeated-color resource lifetime and shared-material protection. Browser benchmark measured 1000 physics steps in 822.2 ms; its Canvas preview timing is CPU drawing only, not a displayed-frame-rate claim. Final evidence: `target/tmp/round220-ai-agent/gate-clean/overall.log`, `overall.status`, `agent-browser/agent-evidence.json`; frozen source hashes: `target/tmp/round220-ai-agent/final-source.sha256`.

A successful compile alone is not evidence of a working browser tool loop.
