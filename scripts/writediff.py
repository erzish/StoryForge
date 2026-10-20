"""Write-path differential test: same mutations against Node and Rust.

Both servers must start from identical database copies. The script issues the
same write calls to each, then compares (a) the responses and (b) the resulting
SQLite rows, so a handler that answers 200 but persists something different
still fails.

    python scripts/writediff.py --rust http://127.0.0.1:5123 --node http://127.0.0.1:5199 \
        --rust-db ... --node-db ...
"""

from __future__ import annotations

import argparse
import json
import sqlite3
import sys
import urllib.error
import urllib.request

FAILS: list[str] = []


def call(base: str, method: str, path: str, body=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(
        base + path, data=data, headers={"Content-Type": "application/json"}, method=method
    )
    try:
        with urllib.request.urlopen(req, timeout=60) as response:
            return response.status, json.load(response)
    except urllib.error.HTTPError as err:
        try:
            return err.code, json.load(err)
        except Exception:
            return err.code, err.read().decode("utf-8", "replace")


def check(name: str, left, right):
    if left != right:
        FAILS.append(name)
        print(f"FAIL {name}\n     node={left!r}\n     rust={right!r}")
    else:
        print(f"ok   {name}")


def run(base: str, db_path: str) -> dict:
    """Perform the mutation sequence and return everything observable."""
    out: dict = {}

    status, text = call(base, "GET", "/api/chapters/2/text")
    out["get"] = status
    revision = text["revision"]
    new_body = text["body"] + "\n\nДобавка для дифференциального теста."
    out["put"] = call(base, "PUT", "/api/chapters/2/text",
                      {"body": new_body, "expected_revision": revision})
    out["put_stale"] = call(base, "PUT", "/api/chapters/2/text",
                            {"body": "x", "expected_revision": "deadbeef"})

    out["note"] = call(base, "POST", "/api/chapters/2/annotations",
                       {"quote": "Мия", "note": "тестовая заметка", "prefix": "", "suffix": ""})
    out["apply_dry"] = call(base, "POST", "/api/chapters/2/apply-edits",
                            {"dry_run": True,
                             "edits": [{"annotation_id": out["note"][1].get("id"),
                                        "old": "Мия", "new": "Мия"}]})

    out["fact"] = call(base, "POST", "/api/books/1/facts",
                       {"subject_type": "character", "subject_id": 1,
                        "kind": "состояние", "value": "дифф-тест", "from_chapter": 2})

    con = sqlite3.connect(db_path)
    con.row_factory = sqlite3.Row
    chapter = con.execute(
        "SELECT chars_no_space, words, status FROM chapter WHERE number=2").fetchone()
    out["chapter_row"] = dict(chapter)
    out["versions"] = con.execute(
        "SELECT reason FROM chapter_version WHERE chapter_id=2 ORDER BY id").fetchall()
    out["versions"] = [row[0] for row in out["versions"]]
    out["annotation"] = con.execute(
        "SELECT status, note FROM annotation WHERE chapter_id=2 ORDER BY id DESC LIMIT 1").fetchone()
    out["annotation"] = list(out["annotation"]) if out["annotation"] else None
    con.close()
    return out


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rust", default="http://127.0.0.1:5123")
    parser.add_argument("--node", default="http://127.0.0.1:5199")
    parser.add_argument("--rust-db", required=True)
    parser.add_argument("--node-db", required=True)
    args = parser.parse_args()

    # Refuse to run against a database whose book lives in a real folder:
    # chapter saves are file writes, and a shared root_path rewrites the manuscript.
    roots = {}
    for label, db_path in (("rust", args.rust_db), ("node", args.node_db)):
        con = sqlite3.connect(db_path)
        root = con.execute("SELECT root_path FROM book ORDER BY id LIMIT 1").fetchone()
        con.close()
        roots[label] = root[0] if root else None
        if root and "\\Temp\\" not in root[0] and "/tmp/" not in root[0]:
            print(f"refusing: {label} database book.root_path is {root[0]!r};")
            print("point it at a throwaway copy of the book folder first")
            return 2

    # Chapter saves write files. Sharing one folder makes the second server read
    # the first server's append, which fakes a metric divergence.
    if roots["rust"] == roots["node"]:
        print(f"refusing: both servers share book.root_path {roots['rust']!r};")
        print("give each server its own copy of the book folder")
        return 2

    node = run(args.node, args.node_db)
    rust = run(args.rust, args.rust_db)

    for key in ("get", "put", "put_stale", "note", "apply_dry", "fact"):
        left, right = node[key], rust[key]
        if key in ("put", "put_stale"):
            # revisions legitimately differ; compare ok/error and metrics only
            left_status, left_body = left
            right_status, right_body = right
            check(f"{key} status", left_status, right_status)
            if isinstance(left_body, dict) and isinstance(right_body, dict):
                check(f"{key} keys", sorted(left_body.keys()), sorted(right_body.keys()))
                for field in ("chars_no_space", "words", "unchanged"):
                    if field in left_body or field in right_body:
                        check(f"{key}.{field}", left_body.get(field), right_body.get(field))
                if "error" in left_body or "error" in right_body:
                    check(f"{key}.error", left_body.get("error"), right_body.get("error"))
        elif key == "note":
            check("note status", left[0], right[0])
            check("note.status", left[1].get("status"), right[1].get("status"))
            check("note.quote", left[1].get("quote"), right[1].get("quote"))
        elif key == "apply_dry":
            check("apply status", left[0], right[0])
            check("apply keys", sorted(left[1].keys()) if isinstance(left[1], dict) else left[1],
                  sorted(right[1].keys()) if isinstance(right[1], dict) else right[1])
            check("apply applied", left[1].get("applied"), right[1].get("applied"))
            check("apply rejected", left[1].get("rejected"), right[1].get("rejected"))
        elif key == "fact":
            check("fact status", left[0], right[0])
            check("fact.kind", (left[1] or {}).get("kind"), (right[1] or {}).get("kind"))

    check("chapter row", node["chapter_row"], rust["chapter_row"])
    check("version reasons", node["versions"], rust["versions"])
    check("annotation row", node["annotation"], rust["annotation"])

    print(f"\nchecks failed: {len(FAILS)}")
    return len(FAILS)


if __name__ == "__main__":
    sys.exit(main())
