# Persona: The Batch Commander

**Acceptance test for AMUX-3005 (Board Command Center)**

## Who they are

A founder/operator running 20-50 workers across multiple groups (customers, ops,
GTM, personal). They are the primary source of work for the org — most cards trace
back to their messages. They are also the bottleneck: they hold approvals, make
decisions, and set priorities that workers can't resolve alone.

They are NOT a passive observer. They send work, attend external meetings that
generate more work, and context-switch between direct execution and delegation
constantly throughout the day.

## How they work (the batch pattern)

The defining behavior: **burst, disappear, resurface.**

1. **Burst** (10-20 minutes): rapid-fire 5-15 messages across different workers.
   Each message may contain 1-5 distinct tasks. Some are simple ("send the
   follow-up"), some are compound ("create the account, populate it, integrate it,
   validate the flow"). Some reference prior work ("pick up where you left off on
   X"). Some are corrections ("that card shouldn't be needs:you — you do it").

2. **Disappear** (1-4 hours): back-to-back meetings, phone calls, or deep work on
   something unrelated. Zero visibility into what the workers are doing. Not
   checking the board, not reading transcripts. The org is running without them.

3. **Resurface** (5-10 minutes): they open the board and need to reconstruct what
   happened to everything they sent. They have 3 customer meetings worth of new
   context and a vague memory of what they asked for this morning. The question is
   not "what's on the board" — it's "what happened to the work I sent, and what
   needs me NOW?"

This cycle repeats 3-5 times per day. Each resurface generates a new burst.

## Jobs to be done

### J1: "Every ask has a fate" (Accountability)

After a burst, I need to know: did each message become tracked work? For each thing
I asked for, I want to see:

- The message I sent
- The card(s) it became (or: it became nothing — RED)
- Where each card sits in the pipeline (backlog/doing/review/done)
- Whether the worker is actually working on it or went idle

**Done when:** I can scan a single view and see every message I sent in the last
8 hours mapped to its card(s). Zero unaccounted messages. Any worker holding
un-decomposed input is visibly flagged.

**Failure mode without this:** I message a worker "do X, Y, and Z." The worker
creates one card for X, ignores Y, and starts Z without tracking it. When I
resurface, I see one card and assume everything's handled. Y never happens. Z
happened but has no evidence trail. I only discover this when a customer asks
"where's Y?" two days later.

### J2: "Where does this initiative sit?" (Epic swimlanes)

I have 4-6 active initiatives at any time (a customer onboarding, a feature
launch, a sales push, an internal process fix). Each spans multiple workers and
groups. I need to see one initiative end-to-end: which cards are done, which are
stuck in review, which haven't started, and who owns the stuck ones.

**Done when:** I pick an epic and see a left-to-right flow: backlog -> todo ->
doing -> review -> done -> verified. Cards show their worker. A card jammed at a
gate IS the bottleneck — I don't need to ask "what's blocking this?" because the
gate tells me.

**Failure mode without this:** I know I'm working on "Wexus onboarding." It has
cards across amux-gtm, amux-cloud, backend, and cold-outbound. I open the board
and see 300+ cards sorted by session. I ctrl-F "wexus" and find 6 cards in 4
different statuses across 3 views. I cannot tell if the initiative is 80% done or
10% done. I piece it together manually by reading each card's description, which
takes 10 minutes and yields a fuzzy answer.

### J3: "Am I on top of it or lost?" (State-of-the-org)

The 5-second glance. Not "what's on the board" — "do I need to act RIGHT NOW, and
on what?" This is the difference between "I can go into my next meeting confident"
and "there might be a fire I don't know about."

**Done when:** I see:
- A red list: things that need ME (needs:you, approvals, blocked-on-human
  decisions, idle workers holding my input)
- A green summary: things moving without me (cards advancing through gates, workers
  active, initiatives progressing)
- A count of what's done since I last looked
- Nothing else. No noise, no "12 cards in backlog" that I can't act on.

**Failure mode without this:** I open the board and see 300 cards. 42 are in
backlog. 18 are doing. 7 are in review. I can't tell which of the 7 reviews need
MY review vs peer review that'll resolve itself. I can't tell if the 18 doing cards
are actually progressing or have been stuck for 3 days. I click into 5 cards to
read their descriptions before I find the one that's actually blocked on me. This
takes 8 minutes. I go into my next meeting not confident.

## Anxieties (what keeps them checking)

1. **The silent drop.** I sent a message and it produced nothing — no card, no
   error, no pushback. The worker received it and... what? Did it crash? Did it
   decide my ask was out of scope? Did it decompose it into cards I can't find?
   The worst version: it "did" the work but never tracked it, so the work exists
   somewhere in a transcript but isn't on the board.

2. **The false done.** A card moved to "done" but the work isn't actually complete.
   The worker marked it done because it finished its part, but the whole job
   requires a deploy, or a verification, or a human check that never happened.
   "Done" doesn't mean done — it means "I stopped working on it." The card is
   green, so I don't look at it. The real state is red.

3. **The invisible bottleneck.** An initiative is stuck but nothing looks stuck.
   Worker A is waiting for Worker B's output. Worker B is waiting for a human
   decision. The human decision is buried in a card description that says
   "blocked: need Ethan to approve the email draft." Nobody flagged it as
   needs:you. I only find it when I ask "why hasn't X shipped?"

4. **The stale backlog.** 200 cards in backlog. Are those real work or are they
   noise from auto-capture? Did I send those, or did a worker generate them? Will
   they ever get worked, or are they sitting there making my board look busy while
   nothing happens? The backlog is a guilt pile, not a work queue.

5. **The needs:you misfire.** Tagged needs:you for something the worker should
   handle itself. Every false positive teaches me to ignore the tag. Then a real
   needs:you gets buried. The tag's signal degrades in both directions: too many
   false positives and I stop reading them; too few and real blockers sit
   unescalated.

## What "on top of it" feels like

- I open the board and in 5 seconds I know: 3 things need me, 12 things are
  moving, 4 things shipped since yesterday. I handle the 3 things (an approval, a
  decision, a re-prioritization). Total time: 4 minutes. I close the board
  confident.

- I send a batch of 8 messages. I come back 2 hours later. Every message has a
  card. Two workers finished their tasks. One is blocked and clearly says why. The
  rest are in progress. I handle the blocker. Done. No detective work.

- I'm in a customer meeting and someone asks "where's the integration?" I open the
  epic swimlane and see: 3 of 5 cards done, 1 in review (peer review, will
  resolve itself), 1 blocked on external (NetSuite email validation). I can answer
  in 10 seconds without scrolling.

## What "lost" feels like

- I open the board and see a wall of cards. I don't know which are mine and which
  were generated by workers. I don't know which are stale. I start clicking into
  individual cards to read descriptions. After 10 minutes I have a rough picture.
  I'm late for my next meeting.

- I sent work to 5 workers this morning. One finished. One pushed back (good). One
  went idle (bad — why?). Two... I can't tell. Did they get my message? Are they
  working on it? I peek at their sessions. One is working on something else
  entirely. The other is stuck on a permission error. Neither created a card for
  my ask.

- A customer emails asking about something I thought was done. I search the board.
  The card says "done." I check the actual state — it's not deployed, the test
  didn't pass, or the validation email was never read. The card lied. Now I'm
  doing archeology to figure out what actually happened.

## Acceptance criteria for the three views

### View 1 (Accountability) validates when:

- After a burst of 10 messages, every message appears in the view within 5 minutes
- Each message shows: the card(s) it spawned (or "untracked" flag), each card's
  current gate, and the worker's state (active/idle)
- Workers with un-worked input are flagged without requiring the Commander to
  notice the gap
- The view answers "what happened to the thing I sent to X?" in one scan, not a
  click-through

### View 2 (Epic swimlanes) validates when:

- An initiative spanning 4 workers and 8 cards renders as one horizontal flow
- The bottleneck card is visually obvious (stuck at a gate while others have passed
  it)
- The Commander can answer "how far along is [initiative]?" without reading any
  card descriptions
- An epic with all cards in "verified" is visually DONE — not still taking up
  attention

### View 3 (State-of-the-org) validates when:

- The red list contains ONLY items requiring the Commander's action (not peer
  reviews, not auto-drive work, not things workers can resolve)
- The Commander can distinguish "nothing needs me" from "I haven't checked in a
  while" — the view has a freshness indicator
- The green summary gives confidence ("12 cards advanced, 3 shipped") without
  requiring drill-down
- A new needs:you item appears within 60 seconds of being tagged, not on next
  page load
- The digest is readable in 5 seconds. If it takes 30 seconds, it's too detailed

## Anti-patterns to test against

1. **The board-as-filing-cabinet.** 300 cards, no movement. The board records work
   but doesn't drive it. The Commander opens the board, sees the same wall they saw
   yesterday, and closes it. The board trained them that looking is not useful.

2. **The notification flood.** Every card state change pings the Commander. They
   mute notifications. Now real needs:you items are buried in noise. The signal
   path is dead.

3. **The status theater.** Workers move cards through gates to satisfy the
   auto-drive, not because the work is actually at that stage. "Review" means "I
   don't want to be nudged" not "this is ready for review." The pipeline looks
   healthy but the work is hollow.

4. **The single-card umbrella.** One card covers 5 tasks. It can never honestly be
   "done" because it's not one unit of work. It sits in "doing" for a week,
   accumulating description edits instead of advancing through gates. The Commander
   sees 1 card in progress and thinks 1 thing is happening. 5 things are
   (or aren't).
