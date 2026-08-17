#!/usr/bin/env python3
# UTILITY, not the old Python server: audits Claude Code per-project memory dirs (no amux server dependency).
"""Audit Claude Code per-project memory dirs: is every memory reachable, and is the state diagnosable?

Written by gtm-videos for amux (board GV-644 / AMUX-2446). Backstop for a class of bug where a
memory file still EXISTS and is still CORRECT but is pointed at by neither MEMORY.md nor
memory-archive.md, so nothing loads it and nothing explains why. Nothing is deleted; it becomes
unreachable, and unreachable is what a reader experiences as deleted.

Two assertions, both with their denominator printed, and note they are scoped DIFFERENTLY:

  A. Every pointer in this dir's index AND ARCHIVE resolves FROM THIS DIR. Scoped locally on
     purpose: a pointer the reader cannot open is unopenable regardless of what exists elsewhere.
     This is the assertion that found the severe bug (a lane whose index carried 138 pointers and
     could open 1, its files sitting one directory up). It covered only MEMORY.md until 2026-08-08,
     which left ~440 archive pointers unchecked while B counted them as coverage — the archive was
     trusted to supply reachability without ever being audited for it.
  B. Every memory file is referenced by SOME index, anywhere. Scoped globally, and it was
     scoped locally and WRONG when first written. Files in a cwd-derived pool are shared by many
     lanes while each lane's index is written to its own projects/<slug>/memory, so "absent from
     this dir's index" conflated genuinely-unreferenced with referenced-fine-from-another-project.
     On the mixpeek pool that inflated the orphan count 151 -> 267. Files indexed only elsewhere
     are reported as a note, not a violation: paired with DANGLING in that other dir, they are the
     fingerprint of the split-index bug.
  B also catches the inverse — a file in BOTH index and archive, simultaneously live and retired,
  which a presence-only check cannot see.

Without B, "absent from the index" is ambiguous between "retired on purpose" and "silently lost",
and a state you cannot interrogate is one nobody fixes.

Why the denominators are printed and not just the failures: "0 violations" and "scoped to nothing"
produce identical output. `checked 472, 0 bad` self-refutes at 472=0 where a bare `0 bad` does not.

INFRASTRUCTURE EXCLUSION is load-bearing, not politeness. amux's own memory dir contains
MEMORY.preamble-backup.md and amux.md, the latter opening "CLAUDE-TAG-MEM-MARKER: shared notes for
amux-tagged lanes". Neither is a memory. Without the exclusion this reports a defect on a clean
directory, which is how a good check gets switched off by the second person who runs it.

AUDIT THIS READER BY READING IT, ON A SCHEDULE — do not wait for it to annoy someone. Three reader
bugs have been found here (fragments, one-link-per-line, archive never resolution-checked) and all
three share a failure mode: each made the tool report CLEAN or report the wrong thing, never raise a
false alarm. That is not coincidence, it is selection. A reader bug that OVER-reports gets fixed the
day someone trips over it, so the bugs that survive in any detector are selected for silence. Both
were found by peers reading the source against their own data (general-canvas-apps and creative-dna,
2026-08-08); nobody was burned into finding them, and nothing in the output would have led there.
Corollary for anyone extending this file: the assertions most worth doubting are the ones that have
never failed.

BEFORE YOU BULK-ACT ON THIS TOOL'S OUTPUT, RE-RUN IT. general-canvas-apps' lesson from using it
(2026-08-08), and the most transferable thing in its history: mid-way through a 151-file indexing pass
they appended a file to the archive because it appeared in the orphan list, and it was in that list
only because of the fragment bug below. It had been correctly indexed all along, so the batch
MANUFACTURED the IN-BOTH contradiction the tool then reported. Nobody was careless — the input was
wrong and the action was faithful to it.

The general shape: when someone bulk-acts on a detector's output, a defect in the detector becomes
THEIR defect at the scale of the batch. A one-file bug cost one bad edit here; the same bug spread
across a class would have cost 151. So a detector aimed at bulk remediation owes its users a way to
check itself cheaply, which is what --self-test is for, and the batch owes the detector one fresh run
before it writes. Two reader bugs have been found in this file so far and both made correct markdown
look broken, so treat "the index is wrong" as the less likely explanation until the reader is proven.

Usage:
  memory_index_audit.py                 # audit every project memory dir
  memory_index_audit.py --dir <path>    # audit one
  memory_index_audit.py --self-test     # prove the detector can both fire AND stay quiet
Exit 0 clean, 1 violations found, 2 self-test failed.
"""
import argparse
import glob
import os
import re
import sys
import tempfile

LINK = re.compile(r"\[[^\]]*\]\(([^)]+)\)")
INDEX, ARCHIVE = "MEMORY.md", "memory-archive.md"
SKIP_NAMES = {INDEX, ARCHIVE, "MEMORY.preamble-backup.md"}
SKIP_HEADER = "CLAUDE-TAG-MEM-MARKER"


def pointers(text):
    """[(raw_link, file_path)] for EVERY link on EVERY index line, fragment stripped.

    TWO reader bugs lived here, both the same mistake: my model of an index line was narrower than
    the format, so correct markdown read as broken. Both were found by general-canvas-apps
    (2026-08-08) while bulk-indexing orphans, and in both cases they declined to edit the index to
    satisfy the tool — right call, the index was correct and the reader was wrong.

    ONE POINTER PER LINE. The old pattern was anchored (`^- \\[...\\]\\((...)\\)` with re.M), so
    findall matched once per LINE and then moved to the next line, never the next link. A deliberate
    two-memory entry —
        - [Artifacts have producers](a.md) / [check the WRITER](b.md) — hook
    — hid b.md completely, and b.md then read as referenced by nobody. All three of their remaining
    violations were this shape, every one a link #2 of 2.

    ANCHORS. `foo.md#retention` is a valid pointer at a section; treating the whole string as a path
    broke three call sites at once, and the two directions had opposite signs:
      over-report  A reported DANGLING on correct markdown. Worse than a noisy line, because
                   DANGLING is the SEVERE class in this script's own triage split, so a reader
                   chases an unopenable link that opens fine.
      UNDER-report B compared bare filenames against pointer strings, so an anchored index entry
                   never matched and IN-BOTH could not fire. The one live instance was genuinely
                   both live and retired and this bug HID it; it escaped ORPHANED only because the
                   archive happened to carry an unanchored second reference, i.e. by luck.
    The under-report is the worse half and nobody reported it, because a false negative produces no
    output to complain about.

    Both fixes are normalised HERE rather than at each call site, so a future consumer cannot forget.

    Non-file targets (a bare `#section` self-link, an http(s) URL) return an empty path and are
    dropped by both assertions — they make no claim about a local file. Zero instances of either
    exist today; handled because they are the same cry-wolf shape and cost one line.

    A markdown anchor belongs to the LINK, not to the filename: `foo.md#retention` is a valid,
    openable pointer at a section of foo.md. Treating the whole string as a path broke this in
    THREE places at once, and the two directions had opposite signs:

      over-report  A reported DANGLING on a pointer that is correct markdown. Worse than a noisy
                   line, because DANGLING is the SEVERE class in this script's own triage split,
                   so a reader chases an unopenable link that opens fine. (Found by
                   general-canvas-apps 2026-08-08 while indexing orphans; the pointer was another
                   writer's line and correct — the defect was in the reader.)
      UNDER-report B compares bare filenames against pointer strings, so an anchored index entry
                   never matched and IN-BOTH could not fire. The one live instance
                   (a-zero-means-the-token-is-absent-not-the-thing.md, indexed at #retention and
                   archived whole) was genuinely simultaneously live and retired, and this bug
                   HID it. It also escaped ORPHANED only because the archive happened to carry an
                   unanchored second reference — luck, not correctness.

    The under-report is the worse half and nobody reported it, because a false negative produces
    no output to complain about. Normalising at extraction is what makes it structural: every
    consumer of a pointer gets the same path, rather than three call sites each remembering to strip.

    Non-file targets (a bare `#section` self-link, or an http(s) URL) are returned with an empty
    path and dropped by both assertions — they make no claim about a local file. Zero instances of
    either exist today across all projects; handled because they are the same cry-wolf shape and
    cost one line, not because they were observed.
    """
    out = []
    for line in text.splitlines():
        if not line.lstrip().startswith("- "):
            continue
        for raw in LINK.findall(line):
            path = "" if raw.startswith(("#", "http://", "https://")) else raw.split("#", 1)[0]
            out.append((raw, path))
    return out


def read(p):
    try:
        with open(p, encoding="utf-8", errors="replace") as fh:
            return fh.read()
    except OSError:
        return ""


def is_memory(d, f):
    """A .md in the dir that is an actual memory, not index/archive/infrastructure."""
    if f in SKIP_NAMES:
        return False
    # header sniff, not whole-file: the marker is declared at the top by convention
    return SKIP_HEADER not in read(os.path.join(d, f))[:400]


def all_claims(dirs):
    """filename -> {projects whose index/archive points at it}.

    Needed because assertion B was WRONG when first written (found 2026-08-06 while chasing
    AMUX-2446, after amux surfaced the shared-pool layout). It asked "is this file in THIS
    dir's index?", which assumes a directory's files belong to that directory's index. They
    do not: many lanes share one cwd-derived pool for FILES while each lane's INDEX is written
    to its own projects/<slug>/memory. So "absent from this index" conflated two states —
    genuinely unreferenced, and referenced perfectly well from another project. On the mixpeek
    pool that inflated the orphan count from 151 to 267. Scope the question to every index, and
    report indexed-elsewhere separately, because that is the fingerprint of the cause-2 bug
    (files here, index over there, pointers dangling from where the index lives).
    """
    claims = {}
    for d in dirs:
        proj = os.path.basename(os.path.dirname(d))
        paths = {p for _, p in pointers(read(os.path.join(d, INDEX))) + pointers(read(os.path.join(d, ARCHIVE))) if p}
        for t in paths:
            claims.setdefault(os.path.basename(t), set()).add(proj)
    return claims


def audit(d, claims=None):
    idx, arch = read(os.path.join(d, INDEX)), read(os.path.join(d, ARCHIVE))
    if not idx:
        print(f"{d}\n  no {INDEX} — not a memory dir, skipping")
        return None
    files = sorted(f for f in os.listdir(d) if f.endswith(".md") and is_memory(d, f))
    proj = os.path.basename(os.path.dirname(d))

    # A: pointers resolve FROM WHERE THE INDEX LIVES. Correctly scoped to this dir — a pointer
    # the reader cannot open is unopenable no matter what exists elsewhere. This is the
    # assertion that found the severe bug; keep it local.
    idx_ptrs = pointers(idx)
    file_ptrs = [(raw, p) for raw, p in idx_ptrs if p]  # only these claim a local file
    # A pointer that does not resolve LOCALLY but does resolve at a path the index
    # itself publishes is not the bug this assertion exists to catch. amux now
    # writes a resolution block at generation time (AMUX-2446):
    #     > **Where these memories live.** ...
    #     >   - `/abs/path/to/memory/` (137 entries)
    # so the reader is told where to open them. Counting those as DANGLING would
    # leave the detector permanently red on a directory that is navigable, and a
    # check that stays red after the fix gets switched off — the same way an
    # unexplained failure class in a CI audit gets waved through.
    #
    # Reported as HINTED, separately from both clean and broken, because the
    # underlying split is still real and still worth seeing: it is a working
    # workaround, not an absence of the condition.
    hinted_dirs = re.findall(r"^>\s+-\s+`([^`]+)`", idx, re.M)

    # A COVERS THE ARCHIVE TOO. It did not, and that was the third reader gap in this file — found
    # independently by creative-dna and general-canvas-apps (2026-08-08). A was scoped to MEMORY.md,
    # so ~440 archive pointers were never resolution-checked while B counted those same pointers as
    # coverage. An asymmetry, and in the dangerous direction: the archive was trusted to SUPPLY
    # evidence of reachability without ever being AUDITED for it.
    #
    # "Keep it local" was right and is unchanged. The gap was that the archive is also local, and is
    # also read — being read when the index has no answer is its entire purpose. creative-dna's
    # argument, which is better than a bug report: my own comment justifying A is the argument for
    # widening it.
    #
    # Measured across all project dirs when the gap was closed: 523 archive pointers had never been
    # checked. 57 do not resolve locally, but 49 of those are openable via the index's own resolution
    # block, so only 8 are genuinely dangling, max 3 in any one dir. Targets live in other lanes'
    # ~/.amux/memory/, i.e. the additive-merge class documented at IN BOTH below, and it GROWS with
    # every sync while the only assertion that could see it looked away.
    #
    # A MEASUREMENT NOTE THAT COST A FEATURE, kept because the mistake is more useful than the code:
    # my first pass at these numbers used a plain exists() loop and reported 57 dangling with one dir
    # at 31 of 31. On that figure I wrote an aggregation branch here — "31 from one merge is ONE
    # finding, not 31" — to stop a flood. The flood was an artifact. The script's own logic applies
    # the HINTED path, which absorbs 49 of the 57, and no dir exceeds 3. I had measured with a cruder
    # instrument than the one I was documenting, then written the cruder number into its comments as
    # though the script had produced it. The branch was removed: it was untested (no live input could
    # reach it), and an aggregation that collapses individual pointers is itself a silent cap. When a
    # tool already implements the discrimination you need, measure THROUGH it, not beside it.
    #
    # Report the RAW link (what the file actually says, so the reader can find the line) but resolve
    # the stripped PATH. Anchor-only and URL pointers carry no path and are not file claims.
    def resolve(ptrs):
        unres = [(raw, p) for raw, p in ptrs if not os.path.exists(os.path.join(d, p))]
        hint = [(raw, p) for raw, p in unres
                if any(os.path.exists(os.path.join(h, p)) for h in hinted_dirs)]
        return unres, hint, [raw for raw, p in unres if (raw, p) not in hint]

    arch_file_ptrs = [(raw, p) for raw, p in pointers(arch) if p]
    unresolved, hinted, dangling = resolve(file_ptrs)
    arch_unresolved, arch_hinted, arch_dangling = resolve(arch_file_ptrs)

    # B: is each file referenced anywhere at all? Match on the pointer target, never on a
    # substring of the file — a filename can appear in prose and would read as indexed.
    idx_targets = {p for _, p in file_ptrs}
    arch_targets = {p for _, p in arch_file_ptrs}
    # Which index links were anchored, so IN-BOTH can say WHICH SHAPE it found. Index-at-a-section
    # plus archive-whole-file is a different situation from both-whole-file: one lane promoted a
    # section out of a memory another lane had retired. Same contradiction, different cause, and the
    # triager should not have to open two files to tell them apart.
    anchored = {p for raw, p in file_ptrs if "#" in raw}
    both = [f for f in files if f in idx_targets and f in arch_targets]
    unref = [f for f in files if f not in idx_targets and f not in arch_targets]
    if claims is None:
        claims = {}
    elsewhere = {f: sorted(claims.get(f, set()) - {proj}) for f in unref}
    orphaned = [f for f in unref if not elsewhere[f]]
    remote = [f for f in unref if elsewhere[f]]

    bad = len(dangling) + len(arch_dangling) + len(both) + len(orphaned)
    print(f"{d}")
    print(f"  A. index pointers resolving here:   {len(file_ptrs) - len(unresolved)}/{len(file_ptrs)}"
          + (f"  (+{len(hinted)} openable via the index's own resolution block)" if hinted else "")
          + (f"  [{len(idx_ptrs) - len(file_ptrs)} non-file pointer(s) skipped]"
             if len(idx_ptrs) != len(file_ptrs) else ""))
    print(f"  A'. archive pointers resolving here: {len(arch_file_ptrs) - len(arch_unresolved)}/{len(arch_file_ptrs)}"
          + (f"  (+{len(arch_hinted)} openable via the resolution block)" if arch_hinted else ""))
    print(f"  B. files referenced by some index:  {len(files) - len(orphaned)}/{len(files)}")
    for f in dangling:
        print(f"     DANGLING  {f}  (this index points at a file it cannot open)")
    # Labelled separately from the index's own dangling because MEMORY.md loads every session while
    # the archive is read on demand — not the same urgency, and the reader needs to know which file
    # holds the dead link. Enumerated individually, never aggregated: see the note on measurement in
    # the A' block above for why an aggregation branch was written here and then removed.
    for f in arch_dangling:
        print(f"     DANGLING(archive)  {f}  (the archive points at a file it cannot open)")
        print("                 If its name appears under ~/.amux/memory/, this is the additive merge: a")
        print("                 lane contributed an archive line whose target memory lives in THAT lane's")
        print("                 dir. Not fixable here — editing the shared archive undoes itself on that")
        print("                 lane's next sync (no delete semantics).")
    for f in both:
        shape = ("index points at a SECTION of it, archive retires the WHOLE file"
                 if f in anchored else "both point at the whole file")
        print(f"     IN BOTH   {f}  (simultaneously live and retired; {shape})")
        # Where to fix it, because the obvious edit undoes itself. The shared project archive is
        # built by a purely ADDITIVE merge (amux 5877f38: _add = lines in the LANE's
        # ~/.amux/memory/<lane>.archive.md not already present). There is no delete semantics, so a
        # line removed from the shared file is re-supplied on the owning lane's next sync.
        # Diagnosed by general-canvas-apps after their own first diagnosis (a concurrent writer with
        # a stale copy) turned out to be wrong; verified here by reading the diff.
        print("                 fix in the LANE's ~/.amux/memory/<lane>.archive.md — editing the shared")
        print("                 archive undoes itself on that lane's next sync (additive merge, no delete)")
    for f in orphaned:
        print(f"     ORPHANED  {f}  (no index anywhere references it)")
    if remote:
        print(f"     note: {len(remote)} file(s) here are indexed only by other project(s), "
              f"e.g. {remote[0]} <- {', '.join(elsewhere[remote[0]][:2])}")
        print(f"           not counted as violations; combined with DANGLING there, that pair is "
              f"the shared-pool/split-index signature")
    print(f"  -> {'clean' if not bad else str(bad) + ' violation(s)'}")
    return bad


def self_test():
    """Both directions. A detector that only ever passes is indistinguishable from one that
    cannot fail, so prove it goes RED on a seeded violation and GREEN on a clean dir."""
    ok = True
    with tempfile.TemporaryDirectory() as t:
        # clean: one memory, indexed; one retired, archived; plus infra that must be ignored
        open(os.path.join(t, "a.md"), "w").write("---\nname: a\n---\nbody\n")
        open(os.path.join(t, "b.md"), "w").write("---\nname: b\n---\nbody\n")
        open(os.path.join(t, "amux.md"), "w").write(f"{SKIP_HEADER}: not a memory\n")
        open(os.path.join(t, INDEX), "w").write(f"- [A](a.md) — hook\n- [Archived]({ARCHIVE}) — hook\n")
        open(os.path.join(t, ARCHIVE), "w").write("- [B](b.md) — hook\n")
        print("[self-test] clean dir, expect 0:")
        if audit(t, all_claims([t])) != 0:
            print("  FAIL: cried wolf on a clean directory"); ok = False

        # seeded: c.md exists but nothing points at it -> must fire
        open(os.path.join(t, "c.md"), "w").write("---\nname: c\n---\nbody\n")
        print("[self-test] seeded orphan, expect >=1:")
        if (audit(t, all_claims([t])) or 0) < 1:
            print("  FAIL: missed a seeded orphan — the detector is inert"); ok = False

        # seeded: b.md in BOTH -> must fire
        open(os.path.join(t, INDEX), "a").write("- [B](b.md) — hook\n")
        print("[self-test] seeded in-both, expect >=2:")
        n_both = audit(t, all_claims([t])) or 0
        if n_both < 2:
            print("  FAIL: missed a file listed live AND retired"); ok = False

        # ANCHOR CASES, both directions. Splitting on '#' fixes a false DANGLING; the way that fix
        # goes wrong is by making DANGLING unreachable, and a detector that cannot fire passes every
        # test. So prove the anchored-and-present case is quiet AND the anchored-and-MISSING case
        # still fires. (Regression for general-canvas-apps' 2026-08-08 report.)
        open(os.path.join(t, "d.md"), "w").write("---\nname: d\n---\nbody\n")
        open(os.path.join(t, INDEX), "a").write("- [D at a section](d.md#some-heading) — hook\n")
        print("[self-test] anchored pointer to an EXISTING file, expect no new violation:")
        if (audit(t, all_claims([t])) or 0) != n_both:
            print("  FAIL: cried wolf on a valid anchored link — the false positive is back"); ok = False

        open(os.path.join(t, INDEX), "a").write("- [Gone](missing.md#sec) — hook\n")
        print("[self-test] anchored pointer to a MISSING file, expect it to still fire:")
        n_dang = audit(t, all_claims([t])) or 0
        if n_dang <= n_both:
            print("  FAIL: stripping the fragment blinded DANGLING — the fix broke the check"); ok = False

        # MULTI-LINK LINE. A deliberate two-memory entry must have BOTH links seen. The old anchored
        # pattern matched once per line, so link #2 was invisible and its file read as ORPHANED.
        # Both files exist and only the SECOND is at risk, so if this case regresses the count rises.
        open(os.path.join(t, "e.md"), "w").write("---\nname: e\n---\nbody\n")
        open(os.path.join(t, "f.md"), "w").write("---\nname: f\n---\nbody\n")
        open(os.path.join(t, INDEX), "a").write("- [E](e.md) / [F second on the line](f.md) — hook\n")
        print("[self-test] two links on one index line, expect BOTH seen (no new violation):")
        n_multi = audit(t, all_claims([t])) or 0
        if n_multi != n_dang:
            print("  FAIL: a pointer after the first on a line is invisible — its file reads ORPHANED")
            ok = False

        # ARCHIVE RESOLUTION. A dead pointer in the archive must fire, because A ignored the archive
        # entirely until 2026-08-08 while B counted its pointers as coverage. Seeded in the ARCHIVE
        # only, so if A ever narrows back to MEMORY.md this case is the one that catches it.
        open(os.path.join(t, ARCHIVE), "a").write("- [Retired but gone](vanished.md) — hook\n")
        print("[self-test] archive pointer to a MISSING file, expect it to fire:")
        if (audit(t, all_claims([t])) or 0) <= n_multi:
            print("  FAIL: the archive's pointers are not resolution-checked"); ok = False
    print(f"[self-test] {'PASS' if ok else 'FAIL'}")
    return ok


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", action="append", help="memory dir (repeatable); default = all projects")
    ap.add_argument("--self-test", action="store_true")
    a = ap.parse_args()
    if a.self_test:
        sys.exit(0 if self_test() else 2)
    dirs = a.dir or sorted(glob.glob(os.path.expanduser("~/.claude/projects/*/memory")))
    if not dirs:
        print("no memory dirs found"); sys.exit(0)
    # Assertion B is scoped to EVERY index, so it always needs the global claim map, even when
    # auditing one dir with --dir. Building it from only the audited dirs would reintroduce the
    # exact mis-scoping this map exists to fix.
    claims = all_claims(sorted(glob.glob(os.path.expanduser("~/.claude/projects/*/memory"))))
    total, audited = 0, 0
    for d in dirs:
        r = audit(d, claims)
        if r is not None:
            total += r; audited += 1
    print(f"\n{audited} memory dir(s) audited, {total} violation(s) total")
    sys.exit(1 if total else 0)


if __name__ == "__main__":
    main()
