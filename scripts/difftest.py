"""Differential test: Rust backend vs the reference Node backend.

Both servers must run against identical copies of story.db:

    python scripts/difftest.py --rust http://127.0.0.1:5123 --node http://127.0.0.1:5199

Exit code = number of mismatching endpoints (0 means parity on the probed set).
"""

from __future__ import annotations

import argparse
import json
import sys
import urllib.error
import urllib.request

PATHS = [
    "/api/books",
    "/api/books/1",
    "/api/books/1/chapters",
    "/api/books/1/characters",
    "/api/books/1/relationships",
    "/api/books/1/events",
    "/api/books/1/timeline",
    "/api/books/1/promises",
    "/api/books/1/secrets",
    "/api/books/1/world",
    "/api/books/1/moments",
    "/api/books/1/graph",
    "/api/books/1/facts",
    "/api/books/1/knowledge",
    "/api/books/1/diff?from=1&to=5",
    "/api/books/1/state?chapter=3",
    "/api/books/1/state?chapter=3&full=1",
    "/api/dictionaries",
    "/api/characters/1",
    "/api/characters/1/state?chapter=3",
    "/api/chapters/1/text",
    "/api/chapters/1/versions",
    "/api/chapters/1/annotations",
    "/api/books/1/activity",
    "/api/books/1/style-examples",
    "/api/books/1/ai/history",
    "/api/ai/settings",
    "/api/books/1/text-search?q=%D0%9C%D0%B8%D1%8F&limit=5",
    "/api/books/1/facts/search?q=%D0%9C%D0%B8%D1%8F&limit=5",
]


def fetch(base: str, path: str):
    url = base.rstrip("/") + path
    try:
        with urllib.request.urlopen(url, timeout=30) as response:
            raw = response.read().decode("utf-8", "replace")
            status = response.status
    except urllib.error.HTTPError as err:
        raw = err.read().decode("utf-8", "replace")
        status = err.code
    except Exception as err:  # connection refused, timeout
        return -1, f"<transport: {err}>"
    return status, raw


def compare(left, right, where: str, out: list[str]) -> None:
    if type(left) is not type(right) and not (
        isinstance(left, (int, float)) and isinstance(right, (int, float))
    ):
        out.append(f"{where}: {type(left).__name__} vs {type(right).__name__}")
        return
    if isinstance(left, dict):
        for key in left:
            if key not in right:
                out.append(f"{where}.{key}: missing on rust side")
            else:
                compare(left[key], right[key], f"{where}.{key}", out)
        for key in right:
            if key not in left:
                out.append(f"{where}.{key}: extra on rust side")
        return
    if isinstance(left, list):
        if len(left) != len(right):
            out.append(f"{where}: length {len(left)} vs {len(right)}")
            return
        for index, (a, b) in enumerate(zip(left, right)):
            compare(a, b, f"{where}[{index}]", out)
        return
    if left != right:
        out.append(f"{where}: {left!r} vs {right!r}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rust", default="http://127.0.0.1:5123")
    parser.add_argument("--node", default="http://127.0.0.1:5199")
    parser.add_argument("--only", default=None, help="substring filter for paths")
    args = parser.parse_args()

    paths = [p for p in PATHS if not args.only or args.only in p]
    failures = 0
    for path in paths:
        node_status, node_raw = fetch(args.node, path)
        rust_status, rust_raw = fetch(args.rust, path)

        problems: list[str] = []
        if rust_status != node_status:
            problems.append(f"status {rust_status} vs {node_status}")
        else:
            try:
                node_json = json.loads(node_raw)
                rust_json = json.loads(rust_raw)
            except json.JSONDecodeError:
                if node_raw != rust_raw:
                    problems.append("body differs (not JSON on one side)")
            else:
                compare(node_json, rust_json, "", problems)

        if problems:
            failures += 1
            print(f"FAIL {path}")
            for line in problems[:10]:
                print(f"     {line}")
            if len(problems) > 10:
                print(f"     … {len(problems) - 10} more")
        else:
            print(f"ok   {path}")

    print(f"\nroutes: {len(paths)}   mismatching: {failures}")
    return failures


if __name__ == "__main__":
    sys.exit(main())
