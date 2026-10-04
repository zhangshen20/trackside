#!/usr/bin/env python3
"""Check the shape of docs/friction-log.md.

The hackathon scores the friction log on whether each entry records the task attempted, the
steps taken, expected versus actual result, a severity, the workaround and a suggestion. This
script fails (exit 1) when any entry under a dated heading (`## YYYY-MM-DD`) lacks one of the
seven fields, has them out of order, or gives a severity outside low, medium and high. It
passes otherwise. Standard library only.

An entry is a `### Title` block under a dated heading. Its fields are bullets of the form
`- **Task:** text` (or `- **Task**: text`), one per field, in this order:

    Task, Steps, Expected, Actual, Severity, Workaround, Suggestion

    python3 scripts/lint-friction.py                  # docs/friction-log.md
    python3 scripts/lint-friction.py path/to/log.md
"""

import re
import sys
from pathlib import Path

FIELDS = ["Task", "Steps", "Expected", "Actual", "Severity", "Workaround", "Suggestion"]
SEVERITIES = {"low", "medium", "high"}

DATED = re.compile(r"^## (\d{4}-\d{2}-\d{2})\s*$")
HEADING2 = re.compile(r"^## ")
ENTRY = re.compile(r"^### (.+?)\s*$")
FIELD = re.compile(r"^- \*\*([A-Za-z]+):?\*\*:?\s*(.*)$")


def lint(text):
    """Returns (entries checked, list of error strings)."""
    errors = []
    entries = 0
    date = None  # the dated section we are in, or None
    entry = None  # (date, title, [(label, value)]) for the entry being read

    def close():
        nonlocal entry
        if entry is None:
            return
        day, title, fields = entry
        name = f"{day} / {title}"
        labels = [label for label, _ in fields]
        if labels != FIELDS:
            missing = [f for f in FIELDS if f not in labels]
            extra = [f for f in labels if f not in FIELDS]
            dupes = sorted({f for f in labels if labels.count(f) > 1})
            if missing:
                errors.append(f"{name}: missing field(s) {', '.join(missing)}")
            if extra:
                errors.append(f"{name}: unknown field(s) {', '.join(extra)}")
            if dupes:
                errors.append(f"{name}: repeated field(s) {', '.join(dupes)}")
            if not missing and not extra and not dupes:
                errors.append(f"{name}: fields out of order (got {', '.join(labels)})")
        for label, value in fields:
            if label == "Severity":
                word = re.match(r"\s*([A-Za-z]+)", value)
                level = word.group(1).lower() if word else ""
                if level not in SEVERITIES:
                    errors.append(
                        f"{name}: severity {value.strip()!r} is not one of low, medium, high"
                    )
        entry = None

    for lineno, line in enumerate(text.splitlines(), 1):
        m = DATED.match(line)
        if m:
            close()
            date = m.group(1)
            continue
        if HEADING2.match(line):  # an undated section ends the dated one
            close()
            date = None
            continue
        if date is None:
            continue
        m = ENTRY.match(line)
        if m:
            close()
            entry = (date, m.group(1), [])
            entries += 1
            continue
        if entry is None:
            if line.strip():
                errors.append(
                    f"{date}: line {lineno} is not inside a ### entry: {line.strip()[:60]!r}"
                )
            continue
        m = FIELD.match(line)
        if m:
            entry[2].append((m.group(1), m.group(2)))
    close()
    return entries, errors


def main(argv):
    root = Path(__file__).resolve().parent.parent
    path = Path(argv[1]) if len(argv) > 1 else root / "docs" / "friction-log.md"
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as e:
        print(f"lint-friction: cannot read {path}: {e}", file=sys.stderr)
        return 1
    entries, errors = lint(text)
    if entries == 0:
        errors.append("no entries found under a dated heading")
    for err in errors:
        print(f"lint-friction: {err}", file=sys.stderr)
    if errors:
        print(f"lint-friction: {len(errors)} problem(s) in {path}", file=sys.stderr)
        return 1
    print(f"lint-friction: {entries} entries OK in {path}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
