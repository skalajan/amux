> **HISTORICAL (python era):** this audit predates the Python server's removal (2026-08-09) — `deploy-cloud.yml`, `cloud-image.yml`, `release.yml` and the python steps of `checks.yml` it discusses no longer exist.

# CI/CD audit — 2026-08-06

Measured over the last 100 workflow runs. The headline: **the failures are not
in the contribution gate.** `checks` — the workflow that actually gates commits
— is 25/26 green and runs in ~32s. Every meaningful failure is in the
cloud-deploy family, and they share one root cause.

## Run stats (last 100 runs)

| workflow | ok | fail | median secs |
|---|---:|---:|---:|
| Deploy to cloud.amux.io | 1 | **18** | 18 |
| Deploy amux.io | 14 | 3 | 40 |
| Daily backup — cloud container | 0 | **3** | 12 |
| Cloud image — build & push | 19 | 1 | 169 |
| **checks** (the gate) | **25** | 1 | **32** |
| Cloud recover | 0 | 1 | 136 |
| iOS — Nightly TestFlight | 3 | 0 | 300 |

## Root cause of the noise: one dead host, three workflows

`Deploy to cloud.amux.io`, `Daily backup`, and `Cloud recover` all die in
their **first SSH step** (median 12–18s — they fail before doing any work).
Cause is *not* a firewall or a wrong address, both of which were proposed and
disproved:

- DNS agrees with the workflows (`cloud.amux.io` → the same IP they hardcode).
- The host accepts TCP on 22 and 443 and then closes before the SSH banner —
  `ssh-keyscan` fails identically **from a laptop**, so it is not runner-specific.
- That is a userspace-not-servicing signature (AC-216/AC-229: global OOM, no
  container memory limits, gateway killed at 09:41).

So **18 of the 27 total failures are one sick host reported three times.** They
are not flaky CI; they are a truthful alarm about infrastructure, firing on
every push because `deploy-cloud.yml` triggers on `amux-server.py`.

**Recommendation (Ethan's call):** these three workflows should fail *once*
loudly, not on every push. Either gate the deploy on a reachability probe that
skips-with-notice when the host is down, or pause the workflow until AC-216 is
closed. Fixing the host closes all three.

## Coverage gaps found and closed

`checks` was green while three real defects shipped in one night. Two blind
spots, both now fixed:

1. **JS syntax parsed only the FIRST script block.** `re.search` instead of
   `re.findall` — 2,379 of 1,296,891 bytes. The 1.07 MB dashboard block, where
   every UI bug lives, was never parsed by CI while the step printed green.
   (The local pre-commit hook had been fixed months earlier; the CI copy had
   not — a fix applied to one copy of a duplicated check.)
2. **No check for the deleted-function class.** `node --check` proves a block
   *parses*, not that the names it calls *exist*. Three shipped bugs in one
   night were this exact shape (`switchView` → six deleted notes functions;
   `_gridRestoreLayout` and `wsLoadProfile` throwing on their first saved
   pane). Added `tests/check_client_refs.py`.
3. **Secrets were gated only by a local hook.** AC-239: four credentials
   committed since 2026-03-11 in a public repo, because the pre-commit hook was
   the only gate and its patterns matched none of them. The scan now runs in CI
   too, with the added patterns.

## Principles applied

- **Every check self-tests.** `check_client_refs.py` plants a ghost call that
  must be caught and a real name that must resolve; the CI secret scan plants a
  fake key before scanning. A check that cannot demonstrate it can fail is
  theatre — and the scanner that missed AC-239 was green the whole time.
- **Extraction is asserted, not assumed.** The refs check fails if the client
  extraction yields < 500 KB, so a regex that stops matching can never make the
  check pass vacuously.
- **Speed comes from scope, not from skipping.** `checks` stays ~32s because it
  is syntax + refs + secrets + pytest on one runner with no Docker build. The
  slow workflows (image build 169s, iOS 300s) are correctly out of the
  contribution path.

## Addendum — 2026-08-06, from the herdr merge

**A third failure class exists that the run-stats above hide: GitHub-side
transients.** `checks` went red on the merge commit with `Failed to resolve
action download info. Error: Service Unavailable` — the runner could not fetch
`actions/checkout` at all, so the job died in **Set up job** before any of our
code ran. Green on a plain re-run, no change.

That matters for reading the table: a red `checks` is not automatically a real
failure, and "our commits are failing" can include runs where GitHub could not
start the job. Distinguish by the failing STEP — a failure in `Set up job` is
infrastructure, a failure in a named step is ours. Worth a retry-once policy on
the setup steps if this recurs; not worth building until it does.

## Addendum 2 — 2026-08-06, a THIRD failure class (amux-cloud)

**`Deploy amux.io` is not the sick host, and this document left its 3 failures
unexplained — which is the same hazard as misattributing them.** It is
`pages.yml`: GitHub Pages, `configure-pages` + `upload-pages-artifact`, and
verified here to contain **zero** SSH or host references. It is not among the
14 workflows that reference the cloud host at all.

Its failures are **concurrency cancellations**:

```yaml
concurrency:
  group: pages
  cancel-in-progress: true
```

With pushes minutes apart, in-flight runs get cancelled by the next one. When
the cancel lands *during* the deployment step, the run reports **`failure`**
while the job conclusion is `cancelled` — confirmed on `dd7a163`: run
conclusion `failure`, job `deploy` conclusion `cancelled`, no failing named
step.

    15:31  5e88e012  cancelled   \  same minute — the later push won
    15:31  40e65713  success     /
    15:13  837bded8  failure
    15:05  26ccb2bb  cancelled

**Honest breakdown of the 11 recent failures: ~7 sick host, 3 pages
concurrency, 1 `Set up job` transient. Zero code failures.** That is a better
story than "18 of 27 were the host" — but only while the three classes stay
distinguishable. An unexplained failure class in an audit is how a genuine
Pages break gets waved through next week.

### Constraint on the reachability probe, if it is built

Yes to gating the cloud family, with one requirement that is load-bearing:
**it must not read as success.** A skipped job renders neutral/grey, and grey
reads as fine. If the probe is ever wrong, the deploy silently does not happen
and nothing says so — trading a loud false failure for a quiet false success,
which is strictly worse and is the rule-7 shape.

So: probe, and on unreachable emit an explicit annotation (`SKIPPED: host
unreachable at <host>:22 — not a deploy failure, see AC-231`) and still mark
the run as needing attention rather than green. The verified-gate text already
says this in words ("if e2e infra is unavailable, note why — that is not a
failure"); the workflow should say it in the run.

Note it fixes ~7 of 11 and **does not touch pages**. Anyone expecting it to
clear all the red will conclude the gate is broken.

**Also corrected, both documented wrongly in CLAUDE.md and found by doing a real
restart:** `~/.local/bin/amux-server.py` is a **symlink** to the repo checkout,
so the documented `cp amux-server.py ~/.local/bin/` is a no-op (`cp` refuses,
"are identical"), and the launchd label is **`com.amux.server`**, not
`com.amux.serve` — the documented `launchctl kickstart` fails with
"Could not find service". Both would leave someone believing they had restarted
a server they had not.
