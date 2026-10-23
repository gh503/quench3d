#!/usr/bin/env python3
"""依赖许可审计 + 最小 CycloneDX SBOM（H3 可复现构建）。

用法：
    python3 tools/license_audit.py --write   # 生成 docs/sbom.json
    python3 tools/license_audit.py --check   # 校验 SBOM 与 Cargo.lock 一致
    python3 tools/license_audit.py --self-test

判据：
* 只审计**外部依赖**（`source` 非空）；工作区 crate 自己不算第三方。
* 每个依赖必须声明 license；没有声明直接失败，不用"未知"糊过去。
* 用 SPDX 表达式求值：`OR` 只要有一条允许分支即可，`AND` 必须全部允许，
  `WITH` 只接受 `Apache-2.0 WITH LLVM-exception`。这样
  `MIT OR Apache-2.0 OR LGPL-2.1-or-later` 这类"双许可选其一"不会误报。
* 允许列表是**宽松但明确**的：MIT/Apache/BSD/ISC/Zlib/Unicode/BSL/0BSD/Unlicense/OFL。
  出现 GPL/AGPL 等只有一条路径的许可会失败，交给人判断，不自动放行。
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SBOM = ROOT / "docs" / "sbom.json"

ALLOWED = {
    "MIT",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Zlib",
    "Unicode-3.0",
    "BSL-1.0",
    "0BSD",
    "Unlicense",
    "OFL-1.1",
    "Ubuntu-font-1.0",
}
ALLOWED_EXCEPTIONS = {"LLVM-exception"}

_TOKEN = re.compile(r"\(|\)|\bAND\b|\bOR\b|\bWITH\b|[A-Za-z0-9.+\-]+")


def _tokens(expr: str):
    pos = 0
    out = []
    while pos < len(expr):
        if expr[pos].isspace():
            pos += 1
            continue
        m = _TOKEN.match(expr, pos)
        if not m:
            raise ValueError(f"无法解析许可表达式 {expr!r}，位置 {pos}")
        out.append(m.group(0))
        pos = m.end()
    return out


class _Parser:
    def __init__(self, tokens):
        self.tokens = tokens
        self.i = 0

    def peek(self):
        return self.tokens[self.i] if self.i < len(self.tokens) else None

    def eat(self, expected=None):
        token = self.peek()
        if token is None:
            raise ValueError("许可表达式意外结束")
        if expected is not None and token != expected:
            raise ValueError(f"期望 {expected!r}，实际 {token!r}")
        self.i += 1
        return token

    def parse(self):
        value = self.parse_or()
        if self.peek() is not None:
            raise ValueError(f"许可表达式多余 token：{self.peek()!r}")
        return value

    def parse_or(self):
        value = self.parse_and()
        while self.peek() == "OR":
            self.eat("OR")
            value = self.parse_and() or value
        return value

    def parse_and(self):
        value = self.parse_atom()
        while self.peek() == "AND":
            self.eat("AND")
            value = self.parse_atom() and value
        return value

    def parse_atom(self):
        token = self.eat()
        if token == "(":
            value = self.parse_or()
            self.eat(")")
            return value
        if token == "WITH":
            raise ValueError("WITH 前缺少许可标识")
        value = token in ALLOWED
        if self.peek() == "WITH":
            self.eat("WITH")
            exception = self.eat()
            if token != "Apache-2.0" or exception not in ALLOWED_EXCEPTIONS:
                value = False
        return value


def is_allowed(expr: str) -> bool:
    # SPDX 旧写法 `MIT/Apache-2.0` 与 `MIT OR Apache-2.0` 同义。
    normalized = expr.replace("/", " OR ")
    return _Parser(_tokens(normalized)).parse()


def _metadata() -> dict:
    cmd = ["cargo", "metadata", "--locked", "--format-version", "1"]
    out = subprocess.check_output(cmd, cwd=ROOT, text=True)
    return json.loads(out)


def build_sbom(meta: dict) -> dict:
    root = next((p for p in meta["packages"] if p["name"] == "quench3d"), None)
    version = root["version"] if root else "0.0.0"
    components = []
    for pkg in meta["packages"]:
        if not pkg.get("source"):
            continue
        license_expr = pkg.get("license")
        if not license_expr:
            raise ValueError(f"{pkg['name']} {pkg['version']} 没有声明 license")
        components.append(
            {
                "type": "library",
                "name": pkg["name"],
                "version": pkg["version"],
                "purl": f"pkg:cargo/{pkg['name']}@{pkg['version']}",
                "licenses": [{"expression": license_expr}],
            }
        )
    components.sort(key=lambda c: (c["name"], c["version"]))
    return {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "version": 1,
        "metadata": {
            "component": {
                "type": "application",
                "name": "quench3d",
                "version": version,
            }
        },
        "components": components,
    }


def render(sbom: dict) -> str:
    return json.dumps(sbom, ensure_ascii=False, indent=2, sort_keys=True) + "\n"


def self_test() -> None:
    cases = {
        "MIT": True,
        "MIT OR Apache-2.0": True,
        "MIT/Apache-2.0": True,
        "MIT OR Apache-2.0 OR LGPL-2.1-or-later": True,
        "Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT": True,
        "(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0": True,
        "Apache-2.0 AND MIT": True,
        "GPL-3.0-only": False,
        "MIT AND GPL-3.0-only": False,
        "Apache-2.0 WITH Classpath-exception-2.0": False,
        "LGPL-2.1-or-later": False,
    }
    for expr, want in cases.items():
        got = is_allowed(expr)
        if got != want:
            raise AssertionError(f"{expr!r} 期望 {want}，实际 {got}")


def main() -> int:
    ap = argparse.ArgumentParser()
    group = ap.add_mutually_exclusive_group(required=True)
    group.add_argument("--write", action="store_true")
    group.add_argument("--check", action="store_true")
    group.add_argument("--self-test", action="store_true")
    args = ap.parse_args()

    self_test()
    if args.self_test:
        print("PASS license-audit self-test")
        return 0

    meta = _metadata()
    external = [p for p in meta["packages"] if p.get("source")]
    failures = []
    for pkg in external:
        license_expr = pkg.get("license")
        if not license_expr:
            failures.append(f"{pkg['name']} {pkg['version']}: 没有 license")
        elif not is_allowed(license_expr):
            failures.append(f"{pkg['name']} {pkg['version']}: {license_expr}")
    if failures:
        print("FAIL 许可审计：以下依赖不在允许列表，需要人工确认：", file=sys.stderr)
        for item in failures:
            print(f"  - {item}", file=sys.stderr)
        return 1

    text = render(build_sbom(meta))
    if args.write:
        SBOM.parent.mkdir(parents=True, exist_ok=True)
        SBOM.write_text(text, encoding="utf-8")
        print(f"PASS license-audit：{len(external)} 个外部依赖，已写 {SBOM.relative_to(ROOT)}")
        return 0

    if not SBOM.exists():
        print(f"FAIL SBOM 不存在：{SBOM}", file=sys.stderr)
        return 1
    if SBOM.read_text(encoding="utf-8") != text:
        print(
            "FAIL SBOM 与 Cargo.lock 不一致；跑 `python3 tools/license_audit.py --write` 后提交",
            file=sys.stderr,
        )
        return 1
    print(f"PASS license-audit：{len(external)} 个外部依赖，SBOM 与 Cargo.lock 一致")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
