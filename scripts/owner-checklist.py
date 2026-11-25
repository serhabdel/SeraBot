#!/usr/bin/env python3
"""Turn the open "Owner:" items in tasks/todo.md into one checklist per release.

Usage:
    python3 scripts/owner-checklist.py                 # print to stdout
    python3 scripts/owner-checklist.py --out docs/owner-checklist.md
    python3 scripts/owner-checklist.py --since 0.74.10 # only releases from then on

Only lines that start with "- [ ] Owner:" are listed (the live passes that need a real
mailbox, desktop or phone). Other open items are counted, not listed. Release headings
are "## Release X.Y.Z ..." lines. Continuation lines of an item (indented) are kept.
Nothing is changed in todo.md; tick items there once you have checked them.
"""
import argparse
import datetime
import re
import sys

HEADING = re.compile(r"^## Release (\d+(?:\.\d+)+)\b(.*)$")
OPEN = re.compile(r"^- \[ \] (.*)$")


def version_key(text):
    return tuple(int(p) for p in text.split("."))


def parse(lines):
    """Return [(version, title, [owner item text], other_open_count)] in file order."""
    releases = []
    current = None
    item = None
    for raw in lines:
        line = raw.rstrip("\n")
        m = HEADING.match(line)
        if m:
            current = [m.group(1), m.group(2).strip(" —-"), [], 0]
            releases.append(current)
            item = None
            continue
        if current is None:
            continue
        o = OPEN.match(line)
        if o:
            body = o.group(1)
            if body.startswith("Owner:"):
                item = [body[len("Owner:"):].strip()]
                current[2].append(item)
            else:
                item = None
                current[3] += 1
            continue
        if item is not None and line.startswith("  ") and line.strip():
            item.append(line.strip())
        else:
            item = None
    return [(v, t, [" ".join(parts) for parts in items], other) for v, t, items, other in releases]


def render(releases, since):
    today = datetime.date.today().isoformat()
    out = [f"# Owner live-pass checklist ({today})", ""]
    shown = 0
    for version, title, items, other in sorted(releases, key=lambda r: version_key(r[0]), reverse=True):
        if since and version_key(version) < version_key(since):
            continue
        if not items:
            continue
        shown += len(items)
        out.append(f"## {version}" + (f": {title}" if title else ""))
        out.extend(f"- [ ] {text}" for text in items)
        if other:
            out.append(f"  (and {other} other open item{'s' if other != 1 else ''} in tasks/todo.md)")
        out.append("")
    if shown == 0:
        out.append("Nothing is waiting for a live pass.")
        out.append("")
    out.append(f"{shown} item{'s' if shown != 1 else ''} to check.")
    return "\n".join(out) + "\n"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--todo", default="tasks/todo.md")
    ap.add_argument("--out")
    ap.add_argument("--since")
    args = ap.parse_args()
    with open(args.todo, encoding="utf-8") as fh:
        text = render(parse(fh.readlines()), args.since)
    if args.out:
        with open(args.out, "w", encoding="utf-8") as fh:
            fh.write(text)
    else:
        sys.stdout.write(text)


if __name__ == "__main__":
    main()
