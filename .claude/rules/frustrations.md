# Log amux-level friction to `frustrations.md`

You run *inside* amux. When amux itself gets in your way — a command that lies, a
notice that misattributes, a gate you cannot satisfy honestly, a probe that cannot
express the answer, a nudge that fires forever — **append an entry to
`frustrations.md` at the repo root.**

This is not a diary. It is the input to deciding what to fix next, so it has to be
greppable and it has to be honest about cost.

## When to log

Log it when amux cost you something you would not have paid with a better harness:

- a command reported success and did nothing, or reported the wrong thing
- an instrument could not express the failure you were looking at
- a gate could not be satisfied truthfully, so the honest move was to stop
- a notice/nudge sent you at the wrong card, the wrong session, or fired forever
- you had to leave the sanctioned path (raw curl, manual edit) to get work done
- two components disagreed about the same fact

Do **not** log: your own mistakes with no amux involvement, one-off environment
noise, or anything you fixed in the same breath with no cost to anyone. A frustration
is friction the NEXT session will also hit.

## How to log it

Append at the bottom. Never rewrite someone else's entry — add a new one that
supersedes it and say so. One entry per distinct friction; if it has two causes it is
two entries.

Use the field block exactly as written in `frustrations.md`'s own header — the fields
are fixed so `grep '^STATUS: open'` and `grep '^AREA: cli'` work. If you invent a
field, nobody's grep finds it.

**Link the card.** A frustration without a `CARD:` is a complaint; with one it is a
work item someone can pick up. If there is no card yet, file one.

**Record the COST in what it actually cost** — minutes, a wrong conclusion shipped, a
push blocked, a card closed that should not have been. "Annoying" is not a cost.

## Then act on it

Logging is not the fix. If the friction is cheap to fix and it is yours to fix, fix
it and set `STATUS: fixed` with the sha. If it belongs to another session's
subsystem, file the card and route it to them. The file exists so the pattern across
entries becomes visible — three entries with `AREA: attribution` is an argument that
one thing needs rebuilding, which no single entry makes on its own.
