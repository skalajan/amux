---
description: Gut-check for every new feature or enhancement in amux. Read before building.
---

# The amux ethos

**The harness gets better as the models get better. Get out of the model's way.**

amux is scaffolding around a model, not a cage for it. Every feature either compounds
with model capability or fights it. This file is the gut check. Each rule below is
here because it was violated in this repo and cost something real.

Run the checks before you build, and again before you call something done.

---

## 1. Does capability reach the model, or only exist?

A feature the model is never handed does not improve when the model improves.

`mcp.json` shipped six MCP servers. The launcher only passed `--mcp-config` when
`CC_MCP=chrome`, and **0 of 101 sessions had `CC_MCP` set**. Six configured servers
reached no agent at all, for months. MCP is the single biggest lever for amux getting
better as models get better at tool use, and it was wired to nothing.

**Check:** who actually receives this, by default, without opting in? An extension
point nobody is enrolled in is decoration. Prefer opt-OUT over opt-IN for anything
that expands what a session can do.

## 2. Are you calling the model for something you could just compute?

Getting out of the model's way is not the same as calling it more often.

Auto-creating a board card labelled every task with `claude -p`. That pays a full CLI
boot, roughly 12-15k input tokens, for a three-word label. It was the most wasteful
per-call touchpoint in the 07-13 token audit, so it got throttled to one per ten
minutes per session, which is why most commands never reached the board at all. The
fix was not a bigger budget. It was deriving the title from the prompt's own first
clause, for free, and letting the model improve it later if it wants to.

**Check:** is the model doing judgment here, or string manipulation? Spend model calls
on judgment. A throttle on a model call is usually a signal that the call was wrong.

## 3. Can the model comply honestly, or does the design force a lie?

Constraints are good. Constraints that cannot be satisfied truthfully are corrosive,
because a capable model will find a way to satisfy them anyway.

Board gates derive from item type. Anything typed `code` is gated on "Implemented and
merged" and "Tests / lint pass". **1,143 of 1,215 open cards were typed `code`**,
including cards that were pure decisions awaiting a human and contained no code. The
only exits were `--force`, a false acknowledgement, or rot.

The design that works: make the escape honest. When a gate does not fit, the fix is to
correct the item's **type**, not to bypass the gate. Fix the type, not the truth.

**Check:** for every constraint, is there a truthful path forward in every legitimate
state? If not, the constraint will teach the model to assert things that are not true.

## 4. Would a wrong answer be detectable from the data you keep?

> A diagnosis being IMPOSSIBLE from the available data IS the bug.

A schedule appeared to re-fire three times in 100 minutes. It had not. Two of the
three were hand-pressed Run-now taps, but `schedule_runs` recorded no source, so a
manual run and a cron fire were byte-identical rows. The reporting session reached the
only conclusion the data supported, and it was wrong. The defect was not the
scheduler. It was that the instrument could not express the discriminator.

There is a second layer, and it is easier to miss. Once `source` existed, it lived in
a database column the consuming session had no reason to poll, while the delivered
message stayed identical. **A tag in a store the reader never opens is the same
failure as no tag.** The blindness just moves.

**Check:** when this goes wrong, what will someone see? Then: will they see it *where
they already look*? Verify from the consumer's vantage, not the producer's.

## 5. Does it accumulate, or does it discriminate?

Automation that appends without deciding degrades as volume grows, no matter how good
the model is.

Every inbound prompt was folded into whatever card a session already had open. One
card reached **451 folded tasks**. At that point it is not a task, it is a journal:
nothing about it is done or not-done, so no gate can govern it, and no model can
reason about it. 421 cards were in that state.

**Check:** at 100x the current volume, is this still coherent? If the answer is "it
becomes a log", it needed to split, not append.

## 6. Is the audit trail real, or just claimed?

The board contract advertised `force` as "bypass (judgment stays with you; **logged**)"
in two separate places. Nothing anywhere logged it. The one escape hatch from the
entire gate system was the one action leaving no trace, while telling you it left one.

An unauditable bypass that claims to be audited is worse than an honest one, because
it gets trusted.

**Check:** grep for the thing the docstring promises. If the promise is not implemented,
either implement it or delete the claim.

## 7. Can your check actually fail?

A green check that cannot detect the bug is theatre, and it is worse than no check,
because it confers false confidence.

Removing the notes feature left `closePeek()` calling a deleted function. The X button
in session peek silently did nothing, and every later click hit an overlay that never
closed. Both standing checks passed the whole time:

- `python -c "import ast; ast.parse(...)"` is **blind to the client**, which lives
  inside a Python string literal.
- `node --check` proves the script **parses**, not that every name it calls **exists**.

The check that finds it: enumerate every function defined in the client, diff against
every function called, and inspect the callers.

**Check after any deletion:** what would still be green if I had broken this? Test the
shipped code path, not a paraphrase of it. Simulating what you believe a function does
cannot catch that function doing something else.

Verification habits do not transfer between operands. A session that learned to
re-read STATUS after the exit-code bug kept re-reading status while its DESC
writes were being silently destroyed twenty times over (desc_append, AMUX-2161)
— the habit gave the feeling of rigour while pointing at the wrong field. Verify
the operand you just wrote, not the one that burned you last time.

A fresh read of the artifact beats being more careful (MG + amux, 2026-08-02):
neither session caught its own error by reasoning — the 12-vs-16 undercount fell
to a re-measure instead of a re-quote, and the false "passes clean" (verdict
tested on a SYNTHETIC shape while the real card carried 33 fold-residue lines)
fell to the pickup notice arriving and being checked against known cards. Test
against the real operand, and when a report arrives, re-read the artifact it
names before defending the code. Related: a right answer via the wrong mechanism
(a prose match latching a CITATION id instead of the dependency) stays right only
until the coincidence lapses — verify the mechanism, not the verdict. And the
session running a test is often not the one holding the discriminating
instrument: three tests in one day were undecidable from the tester's side and
instant from the log-holder's — say so early instead of polling harder.

The sharpest variant: the sanctioned instruction itself can be the theatre. Every
assignment notification told sessions to run `amux board claim <id>`; the command did
not exist, fell through to the help text, and exited 0 — so following the instruction
EXACTLY produced a success signal and no claim (AMUX-2140). When the instruction and
the failure are the same action, no amount of care catches it; only using the result
does. Anything a notification or doc tells an agent to run must itself be exercised.

Make the answer space match the shape of the claim (fleet-converged,
2026-08-02, four instances in one day — orch's MO-3000 the clearest): a prompt
offering exactly `done` or `todo` about a STANDING-ROLE card forces a false
statement either way, and the less-wrong pick (`todo`) recycles the card into
the rot queue forever — rot detection that cannot express "this should not
exist" manufactures permanent rot. Before shipping any N-cell question, ask
which cell a partial, contradictory, or mis-shaped reading lands in; if the
honest answer is "none", the question is missing a cell, and the operator
following instructions literally will never reach the truthful exit.

## 8. Are you deciding something that is the human's to decide?

Getting out of the model's way includes getting out of the user's way.

Twenty-one cards sat in `doing` with no session. The obvious automated fix was to
reassign or discard them. All twenty-one were `owner_type=human`: the user's own
in-flight work. Reassigning or closing them would have been an agent deciding a
person's work was abandoned.

**Check:** whose data is this, and would they recognise the change as theirs? Report
and recommend; do not sweep. Never bulk-delete user content as a side effect of a
refactor.

---

## Applying this

Before building, answer 1, 2, 3 and 5. Before claiming done, answer 4, 6 and 7.
Before touching anything you did not create, answer 8.

If a proposed feature fails one of these, that is not automatically a veto. It is a
signal that the design is carrying a cost you should name out loud in the commit
message, so the next person can weigh it.

**The compounding question, above all of them:** when the next model is meaningfully
better than this one, does this feature get better with it, or does it become the
ceiling?

---

# Known deviations — tracked, not re-discovered

Live places where amux still fights the ethos, found in the 2026-07-30 audit
("any capability that acts as a stop-gap on top of a weaker model needs to not
exist"). Each has a STATUS and an EXIT CONDITION. When you touch one of these
systems, move it toward its exit, and update this section when a row changes.

## D1 — Terminal-scraping as the control plane
14 of 40 compiled regexes parsed Claude Code's rendered UI to infer state. None
improve with a better model; all break when a string changes (the API-error
detector was fixed twice in one day).
**Status: mitigated.** `POST /api/sessions/<n>/report` + global Stop /
UserPromptSubmit hooks let the harness report its own state; a fresh report
outranks the scrape in the status loop. Scrapers remain the FALLBACK (crashes,
subagents, hookless providers).
**Exit:** every consumer reads reported state; scrapers demoted to a
liveness check only.

## D2 — amux answering prompts on the model's behalf
`_RATE_LIMIT_PROMPTS` matches the rate-limit menu and presses 1 fleet-wide — a
scraper pretending to be a user.
**Status: mitigated.** The POLICY is now the human's, set once: pref
`rate_limit_action` = `wait` (default, today's behavior) or `off` (detect but
leave the menu for a human). The scrape stays only because Claude Code exposes
this state nowhere else.
**Exit:** Claude Code exposes rate-limit state via hook/JSON; delete the
pattern table.

## D3 — Hardcoded weak-model helpers
Six call sites pinned `haiku` for helper one-shots. Pinning a weak model is a
bet that cannot improve; the 12–15k-token label call it produced forced a
throttle, which is why most commands never reached the board.
**Status: fixed.** One knob: `AMUX_HELPER_MODEL` / `AMUX_HELPER_MODEL_API` in
`~/.amux/server.env`; all sites read it. (The audit said 5 sites; fixing it
found a 6th.)
**Exit condition met** — the helper tier moves with one line of config.

## D4 — Caps on what the model may see
`_OBS_EVAL_CAP`/`_OBS_STATE_CAP` were code constants — context-scarcity policy
hardcoded where it silently becomes the ceiling as windows grow.
**Status: fixed.** `AMUX_OBS_EVAL_CAP` / `AMUX_OBS_STATE_CAP` in server.env;
defaults unchanged.
**Exit:** revisit defaults upward as model windows grow; policy now lives in
config where that takes one line.

## D5 — Auto-compact at a hardcoded 50%
amux decided WHEN the model should summarize — preempting a judgment models
increasingly make better, with a lossy operation.
**Status: mitigated.** Pref `auto_compact_threshold` (default 50 = today's
behavior; 0 disables the proactive path while keeping resume-dialog handling).
**Exit:** models manage their own context; amux only surfaces the number.

The pattern under all five: amux WATCHED the model and acted on inference. The
durable inverse — the model reporting its own state through a real interface —
is D1's report endpoint; prefer extending it over adding any new scraper.

**D1 exit, extended (2026-08-02):** the board status-request flow
(`POST /api/board/<id>/status-request` -> session authors a status-update onto
the card) is the report endpoint applied to WORK STATUS, not just liveness. The
board is the source of truth because activity flows to it from the session's own
model; amux never scrapes a terminal or summarizes with a pinned helper to fill
a card. It compounds: better model -> better status, no harness change.
