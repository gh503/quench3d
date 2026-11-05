# Live editor AI

The desktop and Web editor share an AI panel, a conversation state machine and the same live command dispatcher. The toolbar entry is “AI 助手”. A goal produces model requests, editor tool calls, actual tool receipts, and follow-up requests until completion, cancellation or error. Normal edits operate on the currently visible scene. HTTP work does not block the editor frame thread.

## Connect

Desktop and Web default to the **AI 助手** tab in the right sidebar. Under **本地模型配置**, add a provider and enter its display name, OpenAI Chat Completions base URL and optional API key. Click **获取可用模型** on that provider to query its `/models` endpoint, choose models from the returned list, then click **添加选中模型**. Models stay within their provider card; configure their temperature, output token limit and timeout there. Providers that cannot list models have a scoped manual-add fallback. Valid edits are saved automatically on the edit frame and loaded on startup; **保存到本机** remains available for an explicit retry. Incomplete invalid input leaves the last saved configuration intact and displays the save error. Select the active provider/model to use it. Fetching, searching and checking catalog rows do not interrupt chat; actual configuration changes cancel the active goal. Editing a provider keeps its card open so complete URLs and keys can be entered normally.

Desktop saves the library to `$XDG_CONFIG_HOME/quench3d/ai.json` (default `~/.config/quench3d/ai.json`), using an atomic write with owner-only permissions on Unix. Web saves it in this browser profile and origin's localStorage under `quench3d.ai.config.v1`. The key is masked in the UI and omitted from agent status/scene exports; the saved local file/browser storage contains the key and is not encrypted. Clearing browser site data removes the Web configuration. Desktop and browser stores are independent local stores, with the same schema and controls.

Web: run `python3 tools/agent_server.py` from the repository or extracted package root, then open `http://127.0.0.1:8765`. Configure the model in the client UI. The same-origin `/api/agent/chat` endpoint only forwards that request's selected client connection; it does not choose or persist credentials/models from server environment variables. Loopback model endpoints may omit the key; cloud endpoints require it. A base path or complete `/chat/completions` endpoint is accepted. A static server alone does not provide the model transport. The proxy binds only to localhost and rejects cross-origin requests.

For Ollama, use provider URL `http://127.0.0.1:11434/v1` and the exact installed model ID, for example `qwen2.5:7b`. For a gateway, enter its OpenAI-compatible API URL, key and actual upstream model ID. Other wire protocols are not currently implemented; a compatible gateway can expose those models through Chat Completions. Existing CLI environment configuration remains available for CLI use, separately from editor configuration.

A useful first goal is: “创建半径 0.4 米的红球，放在 [0,3,0]，聚焦它，运行仿真。” Tool support by the chosen model is required.

## Conversations and history

The right sidebar supports **新建会话**, history selection, and continuous follow-up messages within the current conversation. User/assistant messages appear separately; tool calls and actual receipts can be expanded. Conversation titles come from the first user prompt. Drafts, messages, tool receipts and errors are saved locally and restored on startup. Switching conversations cancels in-flight work; restoring history never replays scene tools. Continue with a new message to inspect and act on the current scene.

Native history uses `ai-history.json` beside `ai.json`, with atomic owner-only writes. Web history uses `quench3d.ai.history.v1` in the same browser profile and origin. Provider credentials remain in the separate model configuration. History is local, independent between Native/Web, and contains the user's conversation and tool output. The history store is limited to 16 MiB; save failures are displayed without silently dropping conversations.

The Chat Completions parser accepts tool-call responses with omitted, null or empty content and text content segments from compatible gateways. Truncated, filtered, refused, empty reasoning-only and unsupported Responses API replies produce explicit errors rather than claiming execution success.

## Current tools and contracts

`inspect_scene`, `spawn_primitive`, `set_transform`, `set_shape`, `set_color`, `delete_object`, `select_object`, `focus_object`, `set_playback`, `step`, `undo`, `redo`.

Targets use unique exact object names. Ambiguous or absent names fail explicitly. Inspect returns the actual scene, physical body positions, shape capabilities and history availability. Mutations return the new revision and actual success/error result. All tools except inspection and playback require `expected_revision`; pause before inspecting and editing. A stale revision triggers a fresh inspection rather than a guessed retry. Revisions are state fingerprints, not access tokens or monotonically increasing sequence numbers.

Primitive dimensions are physical dimensions in meters, limited to 0.0001 through 10000. Position and rotation edits preserve visual shape dimensions and unrelated body handles and velocities. Model members, generated objects, compound bodies and hierarchies need dedicated structure tools and are currently rejected for editing. Operations requiring scene history reject content that the existing scene serializer cannot represent. Undo/redo restores scene edits, not simulation velocity or contact caches.

Tools execute at most once per frame. A response may contain at most 16 tool calls; there is no fixed request-round limit. Execution continues until completion, stop or error. Large request histories are automatically compacted into a bounded historical summary plus recent complete tool-call/receipt groups; the full transcript remains in local history. Compaction is deterministic text reduction, not a separate model summarization request, and the UI displays its size reduction. A summary cannot establish a fresh scene revision: inspect the current scene before further edits. Malformed arguments, duplicate call IDs, invalid response roles and incomplete protocol responses fail explicitly. Tool receipts retain the original tool-call ID. Standard assistant messages with `content: null` and tool calls are preserved through subsequent requests.

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
