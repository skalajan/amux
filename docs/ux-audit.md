# UX interaction audit — 2026-08-06

Every user interaction in the dashboard client, enumerated programmatically
(inline `onclick/onchange/onsubmit/oninput/onkeydown` handlers plus their
resolved handler functions, feedback classified by what the body — and its
callees, two levels deep — actually shows the user). Script:
`scratchpad/uxaudit.py` in the session workspace; re-run it after UI changes,
the numbers below are a snapshot.

## Inventory (snapshot at v0.9.491)

| measure | count |
|---|---|
| unique interactions | **619** |
| onclick | 514 |
| onchange | 41 |
| oninput | 37 |
| onkeydown | 26 |
| onsubmit | 1 |

Feedback classes (an interaction can be in several):

| class | count | meaning |
|---|---|---|
| dom | 559 | writes innerHTML/textContent/class/style the user can see |
| render | 266 | triggers a re-render or refetch |
| toast | 217 | shows a toast (or alert banner) |
| confirm | 207 | asks before acting (confirm/prompt/modal) |
| nav | 180 | changes view/peek/location |
| **NONE detected** | **46** | no feedback the classifier could find |

## The two defect classes that matter

### 1. Confirm-then-silent (the "deleting a schedule nothing is displayed" class)

Asks permission, performs a destructive mutation, then says NOTHING on success —
the list just quietly re-renders. The gap between "it asked me" and "did it
work?" is exactly where trust erodes:

- `deleteSchedule` — Ethan's reported example. **Fixed: success toast + row animates out.**
- `deleteEvent` (calendar) — **fixed, toast.**
- `_jrnlDelete` (journal entry) — **fixed, toast.**
- `deleteLayoutPreset` (workspace preset) — **fixed, toast.**
- `setFilesSessionDir` — audit FALSE POSITIVE: it already shows inline "✓ Set!" label
  feedback, which principle 2 prefers over a toast. Left as is.

### 2. Mutating with no toast AND no confirm (38 handlers)

POST/PATCH/DELETE with neither a question before nor a statement after. Not all
are equal — many show inline DOM feedback next to the control (e.g. `_scopeSave`
writes into a status span, `_sqlRun` renders a result grid, the browser-driver
`_bw*` family repaints a screenshot), which is often BETTER than a toast.
The ones with genuinely nothing, now fixed with toasts:
`_torrentRemove`, `_dictDelWord`, `deleteInvite`. The rest of that shortlist
(`_torrentAdd`/`_torrentAction`, `pullFromRemote`, `_tunnelSettingsStart/Stop`)
keep partial inline feedback and are queued for their next surface touch.
The rest are catalogued in `uxaudit.json` and should gain feedback when their
surface is next touched — each needs judgment about inline-vs-toast, not a sweep.

### 3. The 46 "NONE detected" handlers

Mostly honest false-positives of the classifier: media controls whose feedback
is the media itself (`_vpTogglePlay`, `_abTogglePlay`, fullscreen/PiP), focus
helpers, and `_btnFire`/`_tapTraceEv` plumbing. Real gaps found and worth
fixing on next touch: `removeDraft` (a draft vanishes with no undo path),
`_termClear`, `closeChipPicker` edge cases.

## Principles applied (and to apply on every future surface)

1. **Every mutation answers.** Ask-before is not tell-after: a `confirm()` does
   not excuse silence on success. Toast on success unless the result is
   visibly rendered inline within ~100ms next to where the user acted.
2. **Feedback belongs where the eyes are.** Inline status (like `_scopeSave`'s
   span) beats a toast; a toast beats nothing. Never both for the same act.
3. **Motion states a fact, never decorates.** Animate the thing that changed
   (a row leaving, a panel opening), 150–250ms, ease-out, and never gate input
   on an animation finishing. `prefers-reduced-motion` disables all of it.
4. **Removal animates out before the re-render swallows it.** A row that
   vanishes on an instant repaint reads as "where did it go / did I misclick";
   160ms of collapse reads as "that one left".

## Motion (motion.dev)

Vanilla `motion` (motion.dev) is loaded from CDN alongside chart.js and exposed
as `window.Motion`; the `_anim()` helper no-ops when the CDN is unreachable
(offline PWA) or the OS asks for reduced motion. Applied, deliberately small:

- **Toasts** slide up + fade in, fade out on dismiss (highest-traffic feedback surface).
- **Schedule row** collapses out on delete, before the list re-renders.
- **Group scope panel** body fades/slides on expand (the accordion Ethan uses most).

Not applied to: view switches (full-viewport fades tax LCP on mobile for zero
information), the board (drag interactions own that surface), terminal panes.
