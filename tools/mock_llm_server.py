#!/usr/bin/env python3
"""假 OpenAI 服务：没有本地大模型时，用它体验 quench3d 的 ask / chat。

    python3 tools/mock_llm_server.py --port 8123 &
    quench3d ask  "斜坡上滚球" --base-url http://127.0.0.1:8123/v1 --model mock
    quench3d chat "球停在哪"   --base-url http://127.0.0.1:8123/v1 --model mock

它不是模型，只是一段脚本化的"模型行为"：
  * 请求里带 tools 且用户提到"停/多高/仿真/物理" → 回一个 quench3d_simulate 的工具调用；
  * 下一次请求里出现 role=tool（工具结果已回喂）→ 回一段自然语言总结；
  * 其它情况 → 直接回一个场景 JSON（给 quench3d ask 用）。

只依赖标准库，用来验证协议、工具循环与校验链路，不评价模型质量。
"""

import argparse
import json
import re
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


SCENE = {
    "format": "quench3d.scene",
    # 格式版本：假模型也得跟上真格式，否则 `ask` 拿到的场景会被校验器拒绝
    "version": 2,
    "name": "假模型生成的场景",
    "gravity": [0, -9.81, 0],
    "ground": {"size": 30, "material": "floor"},
    "materials": {
        "floor": {"color": [0.55, 0.58, 0.62], "roughness": 0.9},
        "wood": {"color": [0.78, 0.48, 0.26], "roughness": 0.55},
        "steel": {"color": [0.70, 0.72, 0.76], "metallic": 0.9, "roughness": 0.28},
    },
    "objects": [
        {"name": "斜坡", "shape": {"type": "box", "size": [5, 0.4, 3]},
         "position": [0, 1.0, 0], "rotation_deg": [0, 0, -12],
         "material": "floor", "body": "static"},
        {"name": "球", "shape": {"type": "sphere", "radius": 0.4},
         "position": [-1.6, 2.4, 0], "material": "steel", "body": "dynamic",
         "density": 1200, "restitution": 0.35, "friction": 0.5},
        {"name": "箱子", "shape": {"type": "box", "size": [0.8, 0.8, 0.8]},
         "position": [1.2, 2.2, 0], "material": "wood", "body": "dynamic",
         "density": 400, "restitution": 0.1, "friction": 0.7},
    ],
    "camera": {"position": [8, 4.5, 8.5], "look_at": [0, 1.0, 0], "fov_deg": 48},
    "lighting": {"preset": "studio", "sun_direction": [-0.4, -1, -0.35], "shadows": True},
    "simulate": {"steps": 240, "dt": 0.0166667},
    "render": {"output": "mock_scene.png", "width": 960, "height": 600, "msaa": 4},
}


def reply(content):
    return {
        "id": "chatcmpl-mock",
        "object": "chat.completion",
        "model": "mock",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": content},
                     "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 20, "total_tokens": 30},
    }


def tool_call(name, arguments):
    return {
        "id": "chatcmpl-mock",
        "object": "chat.completion",
        "model": "mock",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": name, "arguments": json.dumps(arguments, ensure_ascii=False)},
                }],
            },
            "finish_reason": "tool_calls",
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 20, "total_tokens": 30},
    }


BROKEN_SCENE = {
    "objects": [
        {"name": "球", "shape": {"type": "pyramid"}, "body": "flying"},
        {"name": "箱子", "shape": {"type": "box", "size": "big"}, "position": [1, 5]},
    ]
}

STATE = {"requests": 0, "broken_sent": False}


def decide(body, broken_first=False):
    messages = body.get("messages", [])
    has_tools = bool(body.get("tools"))
    tool_msgs = [m for m in messages if m.get("role") == "tool"]
    user_msgs = [m for m in messages if m.get("role") == "user"]
    last_user = user_msgs[-1]["content"] if user_msgs else ""

    # 已经有工具结果回喂 → 用工具报告里的真实数字收尾
    if tool_msgs:
        text = tool_msgs[-1].get("content") or ""
        summary = []
        for line in text.splitlines():
            line = line.strip()
            if not line:
                continue
            # 仿真文本报告里第一行是总览，其余 "名字 dynamic ..." 是逐刚体状态
            if line.startswith("步进") or line.startswith("- ") or "位置" in line:
                summary.append(line)
        if not summary:
            summary = [text.strip().splitlines()[0]] if text.strip() else ["工具已返回结果"]
        return reply("根据引擎的实际仿真结果：\n" + "\n".join(summary[:6]))

    # 带工具且问题涉及物理 → 要求跑仿真
    if has_tools and re.search(r"停|多高|多远|仿真|物理|稳|倒|落下|碰撞", last_user):
        return tool_call("quench3d_simulate", {"steps": 240, "scene": SCENE})

    # --broken-first：第一次故意给一个错场景，好让 quench3d ask 演示自动修复
    if broken_first and not STATE["broken_sent"]:
        STATE["broken_sent"] = True
        return reply("这是场景：\n```json\n"
                     + json.dumps(BROKEN_SCENE, ensure_ascii=False, indent=2) + "\n```")

    # 否则直接给场景（quench3d ask 的路径）
    return reply("这是场景：\n```json\n" + json.dumps(SCENE, ensure_ascii=False, indent=2) + "\n```")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    broken_first = False

    def do_POST(self):  # noqa: N802
        length = int(self.headers.get("Content-Length", 0))
        raw = self.rfile.read(length) if length else b"{}"
        try:
            payload = json.loads(raw)
        except ValueError:
            payload = {}
        STATE["requests"] += 1
        data = json.dumps(decide(payload, Handler.broken_first), ensure_ascii=False).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):  # noqa: N802
        # OpenAI 兼容服务通常有 /v1/models
        data = json.dumps({"object": "list", "data": [{"id": "mock", "object": "model"}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):  # 保持安静
        pass


def main():
    ap = argparse.ArgumentParser(description="假 OpenAI 服务（给 quench3d ask / chat 用）")
    ap.add_argument("--port", type=int, default=8123)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--broken-first", action="store_true",
                    help="第一次返回不合法场景，用于演示 ask 的自动修复")
    args = ap.parse_args()
    Handler.broken_first = args.broken_first
    server = ThreadingHTTPServer((args.host, args.port), Handler)
    print(f"假模型服务已启动：http://{args.host}:{args.port}/v1（Ctrl-C 退出）", file=sys.stderr)
    print(f"  quench3d ask  \"斜坡上滚球\" --base-url http://{args.host}:{args.port}/v1 --model mock", file=sys.stderr)
    print(f"  quench3d chat \"球停在哪\"   --base-url http://{args.host}:{args.port}/v1 --model mock", file=sys.stderr)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\n已停止", file=sys.stderr)


if __name__ == "__main__":
    main()
