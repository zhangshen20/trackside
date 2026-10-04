#!/usr/bin/env bash
# Two checks over README.md and docs/*.md. Exits non-zero on the first kind of problem found,
# after listing every line that trips.
#
#   (a) Every relative link or image target exists in the repository. http(s) and other
#       scheme URLs, mailto and in-page anchors are skipped; fenced code blocks are skipped.
#   (b) None of the words the project keeps out of its docs, whole word, case-insensitive:
#       sportsbet, ladbrokes, bet365, neds, pointsbet, betting, wager, "the odds", and TAB as
#       a brand (followed by a space and a capitalised word, so "tab" the UI word is left
#       alone).
#       docs/friction-log.md and docs/product-feedback.md are left out: they quote tool
#       names and Amazon's wording. A line that quotes the smoke script's own banned-word
#       regex is skipped. The phrases that describe the boundary rather than cross it are
#       stripped before the check: "no betting", "no odds", "no tips", "betting talk",
#       "betting words", "odds-free", and the README's two examples of sponsor names the
#       ingester removes from race and venue names.
#
#   scripts/check-docs.sh            # from anywhere; needs bash and python3
set -uo pipefail
cd "$(dirname "$0")/.."

FILES=(README.md docs/*.md)
status=0

python3 - "${FILES[@]}" <<'PY' || status=1
import os, re, sys
link = re.compile(r'!?\[[^\]]*\]\(\s*<?([^)\s>]+)>?(?:\s+"[^"]*")?\s*\)')
scheme = re.compile(r'^[a-zA-Z][a-zA-Z0-9+.-]*:')
bad = 0
for path in sys.argv[1:]:
    fenced = False
    for lineno, line in enumerate(open(path, encoding="utf-8").read().splitlines(), 1):
        if line.strip().startswith("```"):
            fenced = not fenced
            continue
        if fenced:
            continue
        for m in link.finditer(line):
            target = m.group(1)
            if scheme.match(target) or target.startswith("#"):
                continue
            target = target.split("#", 1)[0]
            if not target:
                continue
            resolved = os.path.normpath(os.path.join(os.path.dirname(path), target))
            if not os.path.exists(resolved):
                print(f"check-docs: {path}:{lineno}: link target {target!r} does not exist", file=sys.stderr)
                bad += 1
print(f"check-docs: links OK in {len(sys.argv) - 1} files" if not bad else f"check-docs: {bad} broken link(s)", file=sys.stderr)
sys.exit(1 if bad else 0)
PY

python3 - "${FILES[@]}" <<'PY' || status=1
import re, sys
EXCLUDE = {"docs/friction-log.md", "docs/product-feedback.md"}
# Describing the boundary is allowed; these are stripped before the search.
ALLOW = [
    "no betting", "no odds", "no tips", "betting talk", "betting words", "odds-free",
    # README: examples of the sponsor names the ingester removes from race and venue names.
    '"Sportsbet Longreach" is Longreach',
    '"TAB ONE POOL Edward Manifold Stakes"',
]
# A doc quoting the smoke script's own banned-word regex is not using the words.
QUOTES_REGEX = re.compile(r"sportsbet\|ladbrokes|re\.compile\(")
WORDS = re.compile(r"\b(sportsbet|ladbrokes|bet365|neds|pointsbet|betting|wager)\b|\bthe odds\b", re.I)
# TAB the brand: the word followed by a space and a capitalised word. Case-sensitive on purpose.
TAB = re.compile(r"\b[Tt][Aa][Bb]\b(?= [A-Z][A-Za-z]+\b)")
bad = 0
for path in sys.argv[1:]:
    if path in EXCLUDE:
        continue
    for lineno, line in enumerate(open(path, encoding="utf-8").read().splitlines(), 1):
        if QUOTES_REGEX.search(line):
            continue
        text = line
        for phrase in ALLOW:
            text = re.sub(re.escape(phrase), " ", text, flags=re.I)
        hits = [m.group(0) for m in WORDS.finditer(text)] + [m.group(0) for m in TAB.finditer(text)]
        if hits:
            bad += 1
            print(f"check-docs: {path}:{lineno}: {', '.join(repr(h) for h in hits)} in: {line.strip()[:120]}", file=sys.stderr)
print("check-docs: wording OK" if not bad else f"check-docs: {bad} line(s) use a banned word", file=sys.stderr)
sys.exit(1 if bad else 0)
PY

exit $status
