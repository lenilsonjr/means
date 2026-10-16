#!/usr/bin/env python3
"""Validate chart templates in charts/: shape, uniqueness, placeholder parents,
code scheme, depth, size and the types of the optional statutory keys and
account identifiers.
Exit 1 on any failure."""
import glob
import json
import os
import sys

CHARTS = os.path.dirname(os.path.abspath(__file__))
ROOTS = {
    "Assets": ("asset", 1000, 1999),
    "Liabilities": ("liability", 2000, 2999),
    "Equity": ("equity", 3000, 3999),
    "Income": ("income", 4000, 4999),
    "Expenses": ("expense", 6000, 7999),
}
BALANCE_SHEET = ("Assets", "Liabilities", "Equity")
# Optional per-account keys and their JSON types.
OPTIONAL_ACC = {
    "description": str,
    "statutory_code": str,
    "tax_treatment": str,
    "intercompany": bool,
    "country": str,
    "iban": str,
    "bic": str,
    "institution": str,
    "location": str,
}
REQUIRED_TOP = ("id", "name", "for", "accounts")
REQUIRED_ACC = ("code", "path", "type", "placeholder")

failures = 0


def fail(msg):
    global failures
    failures += 1
    print("  FAIL", msg)


def check(file):
    print(os.path.basename(file))
    with open(file, encoding="utf-8") as f:
        data = json.load(f)
    for k in REQUIRED_TOP:
        if k not in data:
            fail(f"missing top-level key {k!r}")
    if data.get("id") != os.path.basename(file)[:-5]:
        fail(f"id {data.get('id')!r} does not match file name")
    accounts = data["accounts"]
    n = len(accounts)
    # A full chart (T7) also lists balance-sheet accounts and may hold up to 90.
    full = any(a.get("path", "").split(":")[0] in BALANCE_SHEET for a in accounts)
    most = 90 if full else 80
    if not 45 <= n <= most:
        fail(f"{n} accounts, expected 45-{most}")

    by_path = {}
    codes = {}
    for a in accounts:
        for k in REQUIRED_ACC:
            if k not in a:
                fail(f"{a.get('path')!r}: missing key {k!r}")
        path, code = a["path"], a["code"]
        if path in by_path:
            fail(f"duplicate path {path!r}")
        by_path[path] = a
        if code in codes:
            fail(f"duplicate code {code!r} ({codes[code]!r} and {path!r})")
        codes[code] = path
        segs = path.split(":")
        if any(s != s.strip() or not s for s in segs):
            fail(f"{path!r}: empty or untrimmed segment")
        if len(segs) > 3:
            fail(f"{path!r}: deeper than three levels")
        root = segs[0]
        if root not in ROOTS:
            fail(f"{path!r}: first segment must be one of {', '.join(ROOTS)}")
            continue
        want_type, lo, hi = ROOTS[root]
        if a["type"] != want_type:
            fail(f"{path!r}: type {a['type']!r}, expected {want_type!r}")
        if not (code.isdigit() and len(code) == 4):
            fail(f"{path!r}: code {code!r} is not four digits")
            continue
        c = int(code)
        if not lo <= c <= hi:
            fail(f"{path!r}: code {code} outside {lo}-{hi}")
        if len(segs) == 1 and c != lo:
            fail(f"{path!r}: root code should be {lo}")
        if len(segs) == 2 and c % 100 != 0:
            fail(f"{path!r}: level-2 code {code} should be a multiple of 100")
        if len(segs) == 3:
            parent = by_path.get(":".join(segs[:2]))
            if parent and int(parent["code"]) // 100 != c // 100:
                fail(f"{path!r}: code {code} not within parent's hundred ({parent['code']})")
            if c % 100 == 0:
                fail(f"{path!r}: leaf code {code} should not be a round hundred")

    # parents exist and are placeholders; placeholders have children; leaves do not
    children = {}
    for a in accounts:
        segs = a["path"].split(":")
        if len(segs) > 1:
            parent = ":".join(segs[:-1])
            children.setdefault(parent, []).append(a["path"])
            p = by_path.get(parent)
            if p is None:
                fail(f"{a['path']!r}: parent {parent!r} not listed")
            elif not p["placeholder"]:
                fail(f"{a['path']!r}: parent {parent!r} is not a placeholder")
    for a in accounts:
        if a["placeholder"] and a["path"] not in children:
            fail(f"{a['path']!r}: placeholder without children")
        if not a["placeholder"] and a["path"] in children:
            fail(f"{a['path']!r}: leaf with children")
        for key, want in OPTIONAL_ACC.items():
            if key in a and type(a[key]) is not want:
                fail(f"{a['path']!r}: {key} must be a {'boolean' if want is bool else 'string'}")

    # order: parents before children, codes ascending
    seen = set()
    last = 0
    for a in accounts:
        segs = a["path"].split(":")
        if len(segs) > 1 and ":".join(segs[:-1]) not in seen:
            fail(f"{a['path']!r}: listed before its parent")
        seen.add(a["path"])
        c = int(a["code"])
        if c < last:
            fail(f"{a['path']!r}: code {c} out of ascending order")
        last = c

    ph = sum(1 for a in accounts if a["placeholder"])
    print(f"  ok: {n} accounts, {ph} placeholders, {n - ph} postable, depth <= 3")


for file in sorted(glob.glob(os.path.join(CHARTS, "*.json"))):
    check(file)

if failures:
    print(f"{failures} failure(s)")
    sys.exit(1)
print("all templates valid")
