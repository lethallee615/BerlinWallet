#!/usr/bin/env python3
"""Validate docs/STATUS.md.

Offline (default): checks the document's structure, vocabulary, and references.
Online (--online): also asks GitHub whether every referenced issue exists and
whether every open P0/P1 issue appears in the snapshot.

The script uses only the standard library. It is intentionally strict about the
shape of the tables: an agent parsing STATUS.md should never have to guess.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

STATUS_PATH = Path(__file__).resolve().parent.parent / "docs" / "STATUS.md"

STATUS_VALUES = {
    "todo",
    "in-progress",
    "in-review",
    "live",
    "done",
    "at-risk",
    "blocked",
}
PRIORITIES = {"P0", "P1", "P2"}
RESULTS = {"pass", "fail"}
DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
ISSUE_RE = re.compile(r"#(\d+)")


class CheckError(Exception):
    pass


def read(path: Path) -> str:
    if not path.is_file():
        raise CheckError(f"missing {path}")
    return path.read_text(encoding="utf-8")


def section(text: str, heading: str) -> str:
    marker = f"## {heading}"
    start = text.find(marker)
    if start == -1:
        raise CheckError(f"missing section: {marker}")
    rest = text[start + len(marker) :]
    end = rest.find("\n## ")
    return rest if end == -1 else rest[:end]


def table(text: str, heading: str) -> list[list[str]]:
    """Return data rows (no header, no separator) for the first table."""
    lines = section(text, heading).splitlines()
    rows: list[list[str]] = []
    header_seen = False
    for line in lines:
        stripped = line.strip()
        if not stripped.startswith("|"):
            if header_seen and rows:
                break
            continue
        cells = [cell.strip() for cell in stripped.strip("|").split("|")]
        if all(set(cell) <= {"-", ":", " "} for cell in cells):
            continue
        if not header_seen:
            header_seen = True
            continue
        rows.append(cells)
    if not rows:
        raise CheckError(f"no table rows under {heading}")
    return rows


def refs(value: str) -> list[int]:
    if value.strip() in {"", "—", "-"}:
        return []
    return [int(match) for match in ISSUE_RE.findall(value)]


def require(condition: bool, message: str) -> None:
    if not condition:
        raise CheckError(message)


def validate_offline(text: str) -> tuple[set[int], set[int]]:
    updated = re.search(r"^Last updated: (\d{4}-\d{2}-\d{2})$", text, re.MULTILINE)
    require(updated is not None, "missing `Last updated: YYYY-MM-DD`")
    require(
        DATE_RE.match(updated.group(1)) is not None,
        f"invalid date: {updated.group(1)}",
    )

    mvp = table(text, "MVP definition")
    require(len(mvp[0]) == 5, "MVP table must have 5 columns")
    ids: set[str] = set()
    mvp_refs: set[int] = set()
    for row in mvp:
        require(len(row) == 5, f"MVP row has {len(row)} columns: {row}")
        row_id, requirement, status, issue_cell, evidence = row
        require(bool(row_id), "MVP row has an empty ID")
        require(row_id not in ids, f"duplicate MVP ID: {row_id}")
        ids.add(row_id)
        require(status in STATUS_VALUES, f"{row_id}: bad status {status!r}")
        require(bool(requirement), f"{row_id}: empty requirement")
        require(bool(evidence), f"{row_id}: empty evidence (use `—` if none)")
        if status in {"todo", "in-progress", "in-review", "blocked"}:
            require(
                bool(refs(issue_cell)) or evidence.strip() not in {"", "—"},
                f"{row_id}: open work must name an issue or evidence",
            )
        mvp_refs.update(refs(issue_cell))

    backends = table(text, "Backend coverage")
    require(len(backends[0]) == 7, "backend table must have 7 columns")
    for row in backends:
        require(len(row) == 7, f"backend row has {len(row)} columns: {row}")
        require(bool(row[0]), "backend row has an empty name")
        for value in row[1:6]:
            require(
                value in {"yes", "no", "unit", "partial"},
                f"{row[0]}: coverage value must be yes/no/unit/partial, got {value!r}",
            )

    live = table(text, "Live verification log")
    require(len(live[0]) == 5, "live log must have 5 columns")
    live_refs: set[int] = set()
    for row in live:
        require(len(row) == 5, f"live row has {len(row)} columns: {row}")
        date, flow, backend, result, evidence = row
        require(DATE_RE.match(date) is not None, f"bad live date: {date!r}")
        require(bool(flow), f"{date}: empty flow")
        require(bool(backend), f"{date}: empty backend")
        require(result in RESULTS, f"{date}: result must be pass/fail, got {result!r}")
        require(bool(evidence), f"{date}: empty evidence")
        live_refs.update(refs(evidence))

    open_work = table(text, "Open work by priority")
    require(len(open_work[0]) == 2, "open-work table must have 2 columns")
    tracked: set[int] = set()
    for row in open_work:
        require(len(row) == 2, f"open-work row has {len(row)} columns: {row}")
        priority, issue_cell = row
        require(priority in PRIORITIES, f"bad priority: {priority!r}")
        tracked.update(refs(issue_cell))

    return mvp_refs | live_refs, tracked


def gh(*args: str) -> str:
    try:
        completed = subprocess.run(
            ["gh", *args],
            capture_output=True,
            text=True,
            check=False,
        )
    except FileNotFoundError as error:
        raise CheckError("`gh` is required for --online") from error
    if completed.returncode != 0:
        raise CheckError(f"gh {' '.join(args)} failed: {completed.stderr.strip()}")
    return completed.stdout


def validate_online(referenced: set[int], tracked: set[int]) -> None:
    for number in sorted(referenced):
        gh("issue", "view", str(number), "--json", "number,state")

    open_p0_p1 = json.loads(
        gh("issue", "list", "--state", "open", "--limit", "200", "--json", "number,labels")
    )
    for issue in open_p0_p1:
        labels = {label["name"] for label in issue["labels"]}
        if not ({"priority:P0", "priority:P1"} & labels):
            continue
        require(
            issue["number"] in tracked,
            f"open priority issue #{issue['number']} is missing from Open work by priority",
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--online",
        action="store_true",
        help="also validate issue references and P0/P1 coverage against GitHub",
    )
    args = parser.parse_args()

    try:
        text = read(STATUS_PATH)
        referenced, tracked = validate_offline(text)
        if args.online:
            validate_online(referenced, tracked)
    except CheckError as error:
        print(f"status check failed: {error}", file=sys.stderr)
        return 1

    mode = "online" if args.online else "offline"
    print(f"status check passed ({mode}): {STATUS_PATH}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
