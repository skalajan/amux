#!/usr/bin/env python3
"""Retire a validated frustrations.md entry — carrying its text to the card first.

Why this is a script and not a sed: on 2026-08-09, 35 entries were deleted after
validation and TWO of that day's classes recurred within hours (AF-38). The entries
were gone, so recognising the recurrence AS a recurrence depended on someone
remembering the deletion. AF-38's own recommendation was to carry the SYMPTOM onto
the card before deleting, because the card is where a person hitting it again
actually looks. This does that, every time, instead of relying on whoever runs the
sweep to remember.

Two traps this encodes, both learned the hard way:
  - CARD IS NOT A DELETE KEY. 20 card ids cover several entries each (AF-38 alone
    has three). Entries are addressed by their '## ' HEADING.
  - Entries are separated by the '## ' heading ONLY, not by '---'. A block split on
    '---' silently matches the wrong span; that is why a first attempt deleted 1 of 4.

Usage: frustrations_retire.py "<exact heading substring>" [--dry-run]
"""
import json, re, subprocess, sys, urllib.parse

FILE = 'frustrations.md'
API = 'https://localhost:8824'
SESSION = 'amux-frustrations'

def field(block, key):
    m = re.search(rf'^{key}:\s*((?:.|\n  )+?)(?=\n[A-Z_]+:|\n## |\Z)', block, re.M)
    return m.group(1).strip() if m else ''

def main():
    args = [a for a in sys.argv[1:] if a != '--dry-run']
    dry = '--dry-run' in sys.argv
    if not args:
        print(__doc__); return 2
    needle = args[0]
    src = open(FILE).read()
    head, sep, body = src.partition("\n---\n")
    parts = re.split(r'(?=\n## )', body)
    hits = [i for i, p in enumerate(parts) if needle in (re.search(r'\n## (.+)', p).group(1) if re.search(r'\n## (.+)', p) else '')]
    if len(hits) != 1:
        print(f"REFUSING: heading matched {len(hits)} entries, need exactly 1")
        for i in hits:
            print('   ', re.search(r'\n## (.+)', parts[i]).group(1)[:80])
        return 1
    blk = parts[hits[0]]
    card, sym, cost = field(blk, 'CARD'), field(blk, 'SYMPTOM'), field(blk, 'COST')
    status = field(blk, 'STATUS')
    if status != 'fixed':
        print(f"REFUSING: STATUS is {status!r}, not 'fixed'. Validate first.")
        return 1
    note = ("\n\n=== DELETED-ENTRY TEXT PRESERVED (AF-38's rule) ===\n"
            "Retired from frustrations.md after the originating session validated it. Kept here so a\n"
            "RECURRENCE is recognisable from this card alone, without git archaeology.\n\n"
            f"SYMPTOM: {sym}\n\nCOST: {cost}")
    print(f"card={card}  heading={re.search(chr(10)+'## (.+)', blk).group(1)[:70]}")
    if dry:
        print("--dry-run: not writing"); return 0
    r = subprocess.run(['curl', '-sk', '-X', 'PATCH', '-H', 'Content-Type: application/json',
                        '-H', f'X-Amux-Session: {SESSION}', '-d', json.dumps({"desc_append": note}),
                        f'{API}/api/board/{card}'], capture_output=True, text=True)
    if '"error"' in r.stdout or '"blocked":true' in r.stdout:
        print(f"REFUSING to delete: card write failed -> {r.stdout[:160]}")
        return 1
    del parts[hits[0]]
    open(FILE, 'w').write(head + sep + "".join(parts))
    print(f"retired: text carried to {card}, entry removed")
    return 0

sys.exit(main())
