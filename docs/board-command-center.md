# Board command center — three views that answer "what happened to the work I sent?"

**One board, different views.** The board is the org's shared work; each view below is a
lens on it, not a new surface. They all solve the same problem from a different angle:
*I sent work to a worker — what happened to it?* None is batch-specific; each is a
standing view you can open any time.

**Org model (Ethan's framing, and it is exactly the primitives):** groups = teams ·
workers = employees · the board = shared work · gates = the review pipeline. Left→right
on the board is increasing confidence — a card moves right only as it clears review
(peer review inside the group, CI/CD, prod verification). The further right, the more
sure the work is done to satisfaction.

## The three views

| View | Problem it solves | The amux UX | How it sits in an ideal world |
|---|---|---|---|
| **1. Accountability** ("Roll Call") | Work *sent* ≠ work *tracked* ≠ work *being done*. I message a worker and can't tell if it became a task, or if the worker went idle instead of decomposing it. | A board view that links every human **message → the task(s) it became → its epic → its gate**. Any worker who received work but has **no task**, or is **idle with un-worked input**, glows red. Backed by `GET /api/messages/accountability` (built) + the message→task link below. | Every message a worker receives resolves to **≥1 board card, stamped with the message id**. No worker is ever idle while holding un-decomposed input. "Sent" always has a visible fate — you never wonder where an ask went. |
| **2. Initiative** (epic swimlanes) | Work is scattered across workers and statuses; I can't see a whole *initiative* end-to-end, or where it's stuck. | The board **grouped by epic** instead of by session, columns = the gate pipeline (backlog→todo→doing→review→done→verified). Each initiative is a swimlane flowing left→right; every card shows its worker. A card jammed in a gate *is* the bottleneck, visibly. Epics exist (`type=epic` card + `epic` field, built). | Every card belongs to an epic; an epic is a **team's (group's) initiative** — cross-group epics are possible but explicit, not the default, or "epic" and "group" blur. A glance shows which initiatives are done vs stalled and who owns the stall. |
| **3. State-of-the-org** (the standup digest) | I've sent a lot of work over time and I'm lost on the *aggregate* — what's done, blocked, dropped, who's idle, where do *I* need to act. | A board **rollup**: counts by gate × epic × group, plus a **red list** — idle-with-work, unaccounted messages, gate-jammed cards, `needs:you`. The 5-second read that tells you whether you're blocked and on exactly what. Composes accountability + epics + gates. | The digest is always current. **`needs:you` is the only thing that should ever require you** — everything else drives itself to completion via gates + peer review + the auto-drive. You act on the red list; the org handles the rest. |

## The spine that makes all three *exact*

Today the link from a message to the task it spawned is **0 of 86** — accountability can
only *infer* "did this ask become work" from board-activity timing. The fix: **stamp the
task's id onto the message when a worker opens a card from it** (`cmd_history.card_id`
exists; it is just never set). Small change, and it is the spine: with it, view 1 goes
from "this worker did *something*" to "*this exact ask* became *that exact task*, now in
review." Build this first.

## Where we are NOT there yet: the board doesn't drain to completion

The views above expose the problem; this is the *engine* problem underneath, and it is the
"still not there" you are seeing. Measured 2026-08-12:

- **tubescience: 51 backlog · 0 todo · 0 doing** — a full queue and nothing moving.
- **backend: 220 backlog · 5 todo · 1 doing · 18 review · 10 done** — the drive works the
  5 todos while 220 sit.

Root cause: the auto-drive (`board_drive`) dispatches **`todo`** cards to idle workers,
but **nothing promotes `backlog` → `todo`**. Backlog is a stagnant pool by construction,
so a worker with 200 backlog and 0 todo reads as "idle with a mountain of work." That is
why the board does not feel like it drives every worker to completion — because for most
cards, it doesn't move them at all.

**The missing driver:** a promotion/triage step that pulls from backlog into todo at a
controlled rate (WIP-aware), owned by the worker's own model (it decides what to pull
next), so backlog → todo → doing → review → done actually flows. That is the difference
between a board that *records* work and a board that *drives* it. It pairs with view 3:
"idle worker + non-empty backlog" becomes a red-list item that the promotion driver (not a
human) clears.

## Build order

1. **The message→task link** (`card_id` stamping) — the spine; ~an afternoon.
2. **Accountability view** (view 1) on top of it — the first thing you check after a batch.
3. **Backlog→todo promotion driver** — so the board actually drains (this is the real
   "not there yet").
4. **Epic swimlanes** (view 2) — the view you stare at.
5. **State-of-the-org digest** (view 3) — falls out once 1, 2, and epics exist.
