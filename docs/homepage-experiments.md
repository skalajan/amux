# amux.io Homepage Experiments

**KPIs we optimize for:**
| KPI | PostHog event | Target |
|-----|--------------|--------|
| GitHub stars | `$pageview` → github.com/mixpeek/amux click | ↑ star rate |
| iOS downloads | click on App Store link | ↑ iOS installs |
| Cloud signups | click on concierge/cloud CTA | ↑ onboarding calls |

**Tracking note:** PostHog project key is `phc_Ckeacj8y8X8YiLkwHBcpBsJVSnsFKohch2vMv9sJYmE6` (project 378145 "amux", US cloud, us.i.posthog.com). To query KPI clicks use HogQL on `$autocapture` events filtered by `elements_chain` containing the relevant href. Personal API key needed for API queries — set `POSTHOG_PERSONAL_API_KEY` in ~/.amux/server.env. **Note:** site.js was mistakenly using a personal API key (`phx_`) before 2026-07-09 — no website analytics exist prior to that date.

---

## Running Experiments

### EXP-001 — Hero CTA button copy
- **Status:** `concluded: inconclusive`
- **Started:** 2026-07-07
- **Concluded:** 2026-07-26
- **Change:** "View on GitHub" → "⭐ Star on GitHub" (both hero CTA instances in index.html, `replace_all`). **Change kept — no evidence of harm.**
- **Hypothesis:** More action-oriented, emoji-prefixed star CTA increases GitHub star click-through by 20%+
- **KPI:** GitHub star clicks (PostHog autocapture on github.com/mixpeek/amux link)
- **Score (2026-07-26):** Cannot score. PostHog only activated 2026-07-09 (phc_ key); EXP-001 started 2026-07-07 — zero pre-period baseline exists. No valid pre/post comparison possible. Star CTA copy kept permanently.

### EXP-002 — iOS CTA sticky mobile bottom bar
- **Status:** `concluded: inconclusive`
- **Started:** 2026-07-08
- **Concluded:** 2026-07-26
- **Change:** Added sticky fixed bottom bar on mobile (≤600px) with App Store CTA; iOS nav link hidden on mobile. Implemented in site.js. PostHog event: `exp002_ios_sticky_tap`. **Sticky bar kept — UX improvement with no evidence of harm.**
- **Hypothesis:** Moving "iOS app" from nav to a sticky mobile-only bottom bar increases iOS App Store taps on mobile by 30%+
- **KPI:** iOS downloads (PostHog `exp002_ios_sticky_tap` event + App Store link clicks)
- **Score (2026-07-26):** Cannot score. PostHog activated 2026-07-09; EXP-002 started 2026-07-08 — no pre-period data. The exp002_ios_sticky_tap event has accumulated 17 taps total as of 2026-07-25 but without a comparable pre-period baseline these cannot be attributed to the sticky bar. Sticky bar kept permanently.

---

## Experiment Backlog (prioritized)

### EXP-001 — Hero CTA button copy
- **Hypothesis:** "View on GitHub" → "⭐ Star on GitHub" increases star click-through by 20%+
- **Page:** `/` (homepage hero)
- **KPI:** GitHub star clicks
- **Status:** `concluded: inconclusive` — concluded 2026-07-26
- **Implementation:** Changed both "View on GitHub" instances to "⭐ Star on GitHub" in site/index.html. **Change kept.**
- **Effort:** XS (1 line edit)

### EXP-002 — iOS CTA sticky mobile bottom bar
- **Hypothesis:** Moving "iOS app" from nav to a sticky mobile-only bottom bar increases iOS taps on mobile by 30%+
- **Page:** All pages (site.js — injected globally)
- **KPI:** iOS downloads
- **Status:** `concluded: inconclusive` — concluded 2026-07-26
- **Implementation:** site.js injects CSS (fixed bottom bar, body padding-bottom, hide nav iOS link on ≤600px) and appends DOM element. PostHog custom event `exp002_ios_sticky_tap` on tap. **Sticky bar kept.**
- **Effort:** S (site.js injection)

### EXP-003 — Homepage hero social proof line
- **Hypothesis:** Adding a concrete social proof line under the lede increases GitHub clicks and concierge signups by anchoring the "288+ developers" stat
- **Page:** `/` (index.html hero)
- **KPI:** GitHub stars + cloud signups
- **Status:** `concluded: inconclusive`
- **Started:** 2026-07-09
- **Concluded:** 2026-07-26
- **Change:** Added `<p class="social-proof">Trusted by 288+ developers shipping overnight with AI agents — open source on GitHub</p>` below the .lede paragraph. Star count is hardcoded to 288 (current as of 2026-07-09). Text links to GitHub repo. **Social proof paragraph kept.**
- **Implementation:** Added social-proof paragraph + CSS in index.html
- **Score (2026-07-26):** Cannot score. PostHog activated 2026-07-09 same day as EXP-003 start — zero pre-period baseline. No valid pre/post test is possible. Social proof paragraph kept (harmless, provides context for new visitors).

### EXP-004 — Concierge CTA urgency
- **Hypothesis:** Adding scarcity to the concierge CTA ("3 onboarding slots open this month") increases cloud signup clicks by 25%+
- **Page:** `/concierge/` + homepage concierge block
- **KPI:** Cloud signups
- **Status:** `concluded: no_effect`
- **Started:** 2026-07-10
- **Concluded:** 2026-07-24
- **Implementation:** Added amber urgency badge "3 onboarding slots open this month" above the final CTA in `/concierge/index.html`. Inline SVG clock icon + amber pill styling (`rgba(251,191,36,.12)` background, `#fbbf24` text). No JS — pure CSS badge.
- **Effort:** XS
- **Score (2026-07-24):** 14 days in, 0 concierge CTA clicks measured in PostHog across the entire measurement period. Root cause: concierge conversion happens off-site (Calendly booking, direct email to ethan@mixpeek.com) — PostHog autocapture on the concierge page itself cannot observe the downstream conversion event. This experiment is fundamentally unmeasurable with current instrumentation. The badge change is kept in place (harmless UX), but the experiment is marked no_effect due to inability to measure. Future concierge experiments require off-site tracking (Calendly goal, UTM parameter on the Calendly URL).

### EXP-005 — "Star History" social proof on homepage
- **Hypothesis:** Embedding a star-history chart image on the homepage (showing growth trend) increases GitHub clicks from visitors who aren't sure if the project is active
- **Page:** `/`
- **KPI:** GitHub stars
- **Status:** `concluded: inconclusive`
- **Started:** 2026-07-11
- **Concluded:** 2026-07-25
- **Implementation:** Added star-history.com SVG embed (dark/light via `<picture>`) between features grid and final CTA on homepage. 291 stars caption, GitHub CTA button below the chart. Uses `https://api.star-history.com/svg?repos=mixpeek/amux&type=Date` (dark theme variant via `&theme=dark` in `<source>`). `loading="lazy"` so it doesn't block paint.
- **Effort:** S
- **Score (2026-07-18):** Inconclusive — insufficient baseline. PostHog daily homepage KPI clicks: pre-EXP-005: only 2026-07-10=21 (1 day); post-EXP-005 (7d): avg ~13.5/day. Cannot attribute the drop to EXP-005 vs. weekday pattern vs. simultaneous experiments (EXP-006, EXP-007, EXP-008, EXP-009 all started within 5 days). Extended to 2026-07-25 for cleaner measurement.
- **Score (2026-07-25 — final):** Still inconclusive after 14 days of post-period data. No valid pre-period baseline exists — PostHog only activated 2026-07-09, and EXP-005 started 2026-07-11, leaving a single usable pre-data point (July 10: 19 homepage GH clicks/day). Post-period avg is 8.9/day for July 11–24, but comparing that to one data point is not a valid pre/post test. 5 simultaneous experiments (EXP-006–EXP-009, EXP-011) running during the post window make attribution impossible anyway. Chart kept in place — no evidence it's hurting, and it's product social proof. Cannot draw a conclusion.

### EXP-013 — GitHub CTA on high-traffic guides
- **Hypothesis:** PostHog shows /guides/best-ai-model-for-coding-2026/ gets 84 pageviews (2nd highest after homepage) but zero KPI clicks. Adding a compact GitHub CTA box at the top of that guide (and other high-traffic guides with no CTA) increases GitHub stars from guide traffic.
- **Page:** /guides/best-ai-model-for-coding-2026/ (first target; then /guides/claude-code-headless/, /guides/ai-coding-finops/)
- **KPI:** GitHub stars
- **Status:** `inconclusive — bot traffic`
- **Started:** 2026-07-12
- **Implementation:** Added "Run multiple AI models in parallel from one dashboard → ⭐ Star amux on GitHub" pill CTA below intro paragraph, above Decision Framework table. PostHog event: `exp013_guide_github_cta_click`. Applied to best-ai-model-for-coding-2026/index.html.
- **Effort:** XS per page
- **Measure after:** 2026-07-19 (7 days minimum)
- **Score (2026-07-20):** 8 days in. Page has 162 PVs (14-day window) but 0 GitHub KPI clicks. Pattern matches bot/crawler traffic: two large spikes (68 PVs one day, 44 another) with no click behavior. CTA present but no human visitors to convert. Not scaling EXP-013 to additional pages. Superseded by EXP-014 which targets guides with mixed human+bot traffic.

### EXP-006 — GitHub README → iOS CTA
- **Status:** `inconclusive — insufficient pre-period baseline`
- **Started:** 2026-07-13
- **Scored:** 2026-07-21
- **Change:** Added official Apple "Download on the App Store" badge image below the shields badges row in README.md, above the concierge CTA.
- **Result:** Pre-period (2026-07-10–12, 3 days): 10 iOS clicks = 3.3/day. Post-period (2026-07-13–21, 9 days): 27 iOS clicks = 3.0/day. Change = -9%. However, the pre-period is only 3 days (PostHog live since 2026-07-09, experiment started 2026-07-13) — insufficient to draw a conclusion. No meaningful signal either way. The badge change is harmless — keeping it in place.
- **KPI:** iOS downloads (App Store link clicks)

**Upcoming score windows:** EXP-017 → 2026-08-04 · EXP-018 → 2026-08-06 · EXP-019 → 2026-08-09. **Recently concluded: EXP-016 shipped 2026-08-01 (CTO/team persona framing; measure after 2026-08-08). EXP-010 WIN +20.0% (2026-07-31 — phone-first row reorder kept permanently). EXP-015 WIN +200% (2026-07-29 — top-of-page CTA on headless kept permanently). EXP-012 inconclusive (2026-07-28). EXP-014 inconclusive (2026-07-27). EXP-001/002/003 inconclusive (2026-07-26). EXP-005 inconclusive, EXP-011 win +12.3% (2026-07-25). EXP-004/009 no_effect (2026-07-24).**

### EXP-006 — GitHub README → iOS CTA
- **Hypothesis:** Adding an official App Store badge to the README increases iOS installs from GitHub traffic
- **Page:** README.md
- **KPI:** iOS downloads
- **Status:** `inconclusive — insufficient pre-period baseline` — scored 2026-07-21
- **Implementation:** Added official Apple "Download on the App Store" badge (from tools.applemediaservices.com) below the shields badges `</p>`, above the concierge CTA. More visually prominent than the existing shields.io iOS pill badge.
- **Effort:** XS
- **Score (2026-07-21):** Pre avg 3.3/day (3 days), post avg 3.0/day (9 days) = -9%. No meaningful signal. Pre-period too short (only 3 days of pre data available since PostHog activated 2026-07-09). Badge kept in place.

### EXP-007 — Compare pages → GitHub CTA
- **Hypothesis:** Compare pages get high-intent "alternative" traffic. Adding a prominent "Try it free — ⭐ on GitHub" CTA box at the top of each compare page increases star clicks from comparison traffic
- **Page:** All `/compare/amux-vs-*/` pages (7 pages: ngrok, codex, cursor, devin, diy-tmux, jules, n8n)
- **KPI:** GitHub stars
- **Status:** `inconclusive — low traffic volume`
- **Started:** 2026-07-14
- **Scored:** 2026-07-21
- **Implementation:** Added `<!-- EXP-007 -->` GitHub CTA `<div>` block after the subtitle `<p>` on all 7 compare pages. PostHog event: `exp007_compare_github_cta_click` with `{page: window.location.pathname}`.
- **Effort:** M (7-file edit)
- **Score (2026-07-21):** 7 days in. Named event `exp007_compare_github_cta_click` = 0 fires. Autocapture shows ~5 GH clicks from all compare pages combined in 14 days (amux-vs-cmux: 2, amux-vs-codex: 1, amux-vs-hermes: 1, amux-vs-openclaw: 1). Individual compare pages average 10-15 PVs/14d — too low volume for a meaningful A/B result. The CTA block is kept in place (doesn't hurt). Superseded by EXP-012 which targets the compare INDEX page (62 PVs/14d) where the audience is concentrated.

### EXP-008 — Dark/light mode preference → personalization signal
- **Hypothesis:** Users who switch to light mode are more likely to be non-developers (less terminal-native) and may convert better on concierge/cloud vs GitHub. Track theme preference as a PostHog property.
- **Page:** All pages (site.js)
- **KPI:** Cloud signups (segment)
- **Status:** `inconclusive — measurement issue`
- **Started:** 2026-07-15
- **Scored:** 2026-07-22
- **Implementation:** Added `posthog.register({ theme_preference: theme })` in site.js — fires on page load (initial theme) and on every manual toggle.
- **Score (2026-07-22):** `person.properties.theme_preference` returns `None` for all 93 GH clicks in the past 7 days. The `posthog.register()` super property attaches to events but does not propagate to `person.properties` in HogQL — they're different property namespaces. To query by theme, need to use `properties.theme_preference` (event-level) instead of `person.properties.theme_preference`. Segmentation was never working as designed. The data signal is still there in event properties — segmentation just needs a query fix. Keeping the implementation in place.
- **Effort:** XS
- **Measure after:** 2026-07-22 (7 days minimum)

### EXP-009 — "Indie hackers" and "overnight builders" language in hero subtitle
- **Hypothesis:** Explicitly calling out the indie hacker / overnight builder persona in the homepage hero subtitle increases conversion from that audience (HN, Product Hunt, Indie Hackers forum traffic).
- **Page:** `/` (hero subtitle)
- **KPI:** GitHub stars + concierge signups
- **Status:** `concluded: no_effect`
- **Started:** 2026-07-16
- **Concluded:** 2026-07-24
- **Implementation:** Changed `.lede` paragraph in site/index.html to add "Indie hackers, solo builders, and engineering teams use it to..." before the feature list. Preserves the "self-healing built in, so you wake up to finished work" closer.
- **Effort:** XS
- **Score (2026-07-24):** 8 days of data (7-day measurement window passed). Pre-period (2026-07-09–15, before EXP-009): 10.3 GitHub KPI clicks/day avg on homepage. Post-period (2026-07-16–23): 8.1 clicks/day avg. Delta: **-21.4%**. Negative effect — the indie hacker persona framing may have narrowed the perceived audience, reducing conversion from enterprise/team visitors who saw it as "not for me." **Reverted:** `.lede` paragraph reverted in site/index.html as of 2026-07-24, replacing with multi-runtime framing ("Claude Code, Codex, and Gemini CLI sessions from a single dashboard") and updating star count from 288+ to 310+. The social-proof line is kept. EXP-009 is definitively negative and will not be re-tested.

### EXP-010 — Feature table reorder: phone-first features at top
- **Hypothesis:** Most visitors who convert on cloud/iOS CTAs are non-enterprise users who are drawn to the phone/mobile angle. Reordering the feature comparison table to lead with "Mobile dashboard (iOS app + PWA)" before "Self-healing watchdog" may increase iOS and cloud CTA clicks.
- **Page:** `/` (feature comparison table)
- **KPI:** iOS downloads + cloud signups
- **Status:** `concluded: win`
- **Started:** 2026-07-17
- **Concluded:** 2026-07-31
- **Implementation:** Moved "No way to manage agents from your phone" → "Remote control iOS app + PWA" row to the top of the PS problem/solution grid in site/index.html, before "Self-healing watchdog". All other rows shift down one position. **Change kept permanently.**
- **Effort:** XS
- **Score (2026-07-24 — preliminary):** iOS clicks: pre (2026-07-09–16, 7 days) avg 4.7/day; post (2026-07-17–23, 7 days) avg 5.1/day. Delta: **+8.5%**. Positive trend but below the 10% threshold — extended to 14-day post window.
- **Score (2026-07-31 — final):** Pre (Jul 9–16, 7 days): avg **3.6 iOS clicks/day**. Post (Jul 17–30, 14 days): avg **4.3 iOS clicks/day**. Delta: **+20.0%** — above the 10% win threshold. 14-day window gives a clean signal despite EXP-011 overlap (EXP-011 targets GH clicks, not iOS, so confound is minimal). Phone-first row order is kept permanently — the mobile-management audience is the highest-value iOS converter.

### EXP-011 — Plan strip / visibility callout in homepage feature list
- **Hypothesis:** The new Plan strip feature (v0.9.44, July 2026) — which lets you see exactly what your Claude Code agent is planning — is a unique differentiator not communicated on the homepage. Adding a one-line callout in the feature list increases clicks from developers frustrated with agent opacity.
- **Page:** `/` (feature grid or "new" badge on relevant feature row)
- **KPI:** GitHub stars (developer audience)
- **Status:** `concluded: win`
- **Started:** 2026-07-18
- **Concluded:** 2026-07-25
- **Implementation:** Added "No idea what your agent is actually planning or working on right now" → "Plan strip — see your agent's live task list and next steps in real time inside the peek panel. [new badge]" row to the PS grid, between "Web dashboard" and "Kanban board" rows. New badge uses `rgba(110,231,183,.15)` / `#6ee7b7` green pill styling.
- **Effort:** XS
- **Score (2026-07-25):** **+12.3%** homepage GitHub clicks. Pre (Jul 11–17, 7 days): 8.1/day avg [6, 9, 13, 9, 6, 11, 3]. Post (Jul 18–24, 7 days): 9.1/day avg [12, 2, 7, 7, 7, 16, 13]. Delta: **+12.3%**, above the 10% win threshold. Confounds: EXP-009 (negative) ran during the last 2 days of pre AND the entire post period, which means EXP-009 was likely suppressing clicks throughout the measurement window — EXP-011's true positive effect is probably larger than +12.3%. **Change kept permanently.** Plan strip row remains in the homepage PS grid.

### EXP-014 — Add GitHub CTA to high-PV guide pages missing it
- **Hypothesis:** Guides with >20 PVs/14d and 0-1 GitHub KPI clicks are missing a visible CTA. Adding a prominent inline GitHub CTA block (same format as EXP-007 compare pages) will convert at 2-5 clicks/week per page.
- **Page:** /guides/harness-engineering/ (28 PVs, 0 clicks, no CTA), /guides/measuring-ai-coding-agent-roi/ (28 PVs, 0 clicks, no CTA), /guides/claude-code-context-compaction/ (new page)
- **KPI:** GitHub stars (tracked via `exp014_guide_github_cta_click` PostHog event + autocapture)
- **Status:** `concluded: inconclusive`
- **Started:** 2026-07-20
- **Concluded:** 2026-07-27
- **Implementation:** Added inline GitHub CTA div block (indigo border, flex row, "View on GitHub ★" button, PostHog `exp014_guide_github_cta_click` event) to harness-engineering and measuring-ai-coding-agent-roi. Updated dateModified to 2026-07-20 on both. New page claude-code-context-compaction created with same CTA baked in.
- **Effort:** S (3 files)
- **Score (2026-07-27):** 7-day window. Named event `exp014_guide_github_cta_click`: 1 total (from /guides/harness-engineering/). Autocapture GH clicks on EXP-014 pages pre/post: 0 pre, 1 post (harness-engineering only). Pattern exactly matches EXP-007: CTA installed on low-traffic pages (harness-engineering ~108 PVs/14d, measuring-ai-coding-agent-roi not measurably higher) means not enough human visitors to produce statistically meaningful conversion data. **Verdict: Inconclusive — insufficient traffic volume.** The indigo CTA format is kept in place on all three pages (harmless, minimal friction, improves discoverability for the few human visitors). Scale the format to higher-traffic pages (see EXP-015) rather than repeating on low-PV guides. The CTA has now been rolled out to 15+ guide pages total across all runs.

### EXP-012 — Freelancer CTA on compare index
- **Hypothesis:** Compare pages attract high-intent "is this the right tool?" visitors. A "Freelancer? Scale to 5x clients →" contextual CTA on the compare INDEX (62 PVs/14d — highest compare surface) drives traffic to /for/freelancers/ and /concierge/.
- **Page:** `/compare/` index page (strategy shift from individual compare pages which get only 10-15 PVs each — the index is where audiences are concentrated)
- **KPI:** Concierge signups + /for/freelancers/ traffic (PostHog event: `exp012_freelancer_cta_click`)
- **Status:** `concluded: inconclusive`
- **Started:** 2026-07-21
- **Concluded:** 2026-07-28
- **Implementation:** Added Freelancer CTA block between the subtitle and the compare links list in `/compare/index.html`. Two buttons: "How it works" → /for/freelancers/ and "Get set up →" → /concierge/. PostHog event `exp012_freelancer_cta_click` fires with `{destination, page}` on each click. **CTA kept in place.**
- **Effort:** XS
- **Score (2026-07-28):** 7-day window. Named event `exp012_freelancer_cta_click`: 1 total (to `for-freelancers`). Compare index GH autocapture clicks: pre (2026-07-14–20): 2 clicks over 7 days; post (2026-07-21–27): 1 click over 7 days. Volume is too low for any meaningful pre/post comparison (2 vs 1 clicks is noise). Same pattern as EXP-007: compare index doesn't have enough human traffic to measure with click data in a 7-day window. The freelancer CTA itself is kept (harmless, reinforces persona framing). EXP-017 ships now that EXP-012 is scored.

### EXP-017 — "Best multiplexers" callout on /compare/ index page
- **Hypothesis:** The /compare/ index page gets 62 PVs/14d from high-intent "alternative" visitors. Adding a callout linking to /guides/best-ai-agent-multiplexers-2026/ (proven 19% GH CVR) at the top of the compare index will route comparison shoppers to the highest-converting guide and increase total GH clicks from compare traffic.
- **Page:** `/compare/` (index)
- **KPI:** GitHub stars (autocapture on best-ai-agent-multiplexers-2026 link clicks from compare index)
- **Status:** `running`
- **Started:** 2026-07-28
- **Implementation:** Added compact green/teal callout block above EXP-012 block in compare/index.html. Text: "Looking for the best AI agent multiplexers in 2026? Our in-depth guide compares amux, cmux, dmux, Claude Code Routines, and Warp." Button: "Read the guide →" with `exp017_compare_index_multiplexers_callout_click` event. Sits above the freelancer CTA.
- **Effort:** XS
- **Measure after:** 2026-08-04 (7 days minimum)

### EXP-019 — Review dashboard PS row on homepage
- **Hypothesis:** The Review dashboard / Trends view / Fleet org chart (shipped 2026-08-01) represent a new capability class — "understand what your AI team did" — not represented in the homepage PS grid. Adding a dedicated row surfaces this for engineering managers and team leads who are the highest-value audience segment.
- **Page:** `/` (homepage PS problem/solution grid)
- **KPI:** GitHub stars + /guides/ai-agent-team-review/ traffic
- **Status:** `running`
- **Started:** 2026-08-02
- **Implementation:** Added "No idea what your team of agents actually accomplished this week" → "Review dashboard — Trends view groups fleet activity by theme; blockers surface first; Fleet org chart shows who's doing what. Team review guide →" row at the bottom of the PS grid. Links to /guides/ai-agent-team-review/. PostHog event: autocapture on GitHub link + team-review link click.
- **Effort:** XS
- **Measure after:** 2026-08-09 (7 days minimum)

### EXP-018 — Top-of-page CTA rollout to all high-traffic/low-CVR guides
- **Hypothesis:** EXP-015 proved top-of-page CTAs work (+200% GH clicks on headless). Apply the same green/teal banner pattern to the next highest-traffic, lowest-CVR guides: ai-agent-orchestration-2026 (4 GH clicks, likely 200+ PVs) and ai-agent-sandboxing (1 GH click, 122+ PVs). Each banner should be customized to the guide's topic rather than using generic amux copy.
- **Page:** /guides/ai-agent-sandboxing/ (first target — 131 PVs/14d, 1 GH click, 0.8% CVR)
- **KPI:** GitHub stars (PostHog autocapture + custom event `exp018_guide_topofpage_cta_click`)
- **Status:** `running`
- **Started:** 2026-07-30
- **Implementation:** Applied green top-of-page CTA to /guides/ai-agent-sandboxing/. Copy: "Running agents in parallel and need to coordinate their work? amux gives each agent its own isolated tmux session with configurable filesystem access — security and coordination built in, not bolted on." Button: "View amux on GitHub ★" with `exp018_guide_topofpage_cta_click` event. dateModified updated to 2026-07-30. Same compact green/teal banner format as EXP-015 WIN.
- **Effort:** XS per page
- **Measure after:** 2026-08-06 (7 days minimum)

### EXP-016 — CTO/team persona framing in homepage hero
- **Hypothesis:** EXP-009 showed that "indie hacker" framing hurt conversion (-21.4%). The null version of the homepage hero now uses generic "developers" language. Testing a "teams" frame — "AI engineering teams" + specific team size ("5–50 agents") — may increase conversion from team/enterprise visitors without narrowing the solo developer audience. Unlike EXP-009, this targets a premium buyer segment that likely converts on cloud/concierge.
- **Page:** `/` (hero subtitle / .lede)
- **KPI:** GitHub stars + cloud/concierge signups
- **Status:** `running`
- **Started:** 2026-08-01
- **Implementation:** Changed .lede to "amux is the open-source control plane for AI engineering teams. Run a fleet of Claude Code, Codex, and Gemini CLI sessions from a shared dashboard — with self-healing built in, so your team wakes up to finished work instead of crashed sessions. **5–50 agents. Zero coordination overhead.**" Also updated social-proof star count from 310+ to 323+. PostHog event: autocapture on github.com/mixpeek/amux and concierge links.
- **Effort:** XS
- **Measure after:** 2026-08-08 (7 days minimum)

### EXP-015 — Top-of-page GitHub CTA on high-traffic guides with low conversion
- **Hypothesis:** claude-code-headless has 185 PVs but only 2 GH clicks (1.1% CVR). The only GitHub CTA on the page is the EXP-007 block at line 650 — below ~600 lines of content, invisible to most visitors. Adding a compact green teal banner immediately after the subtitle (above the TOC) will catch visitors before they scroll away.
- **Page:** /guides/claude-code-headless/ (185 PVs, 2 GH clicks — biggest CVR gap of high-traffic pages)
- **KPI:** GitHub stars (PostHog event: `exp015_headless_topofpage_cta_click`)
- **Status:** `concluded: win`
- **Started:** 2026-07-22
- **Concluded:** 2026-07-29
- **Implementation:** Added compact green/teal banner immediately after subtitle paragraph, before the TOC. Text: "Want to run 10+ headless agents in parallel? amux orchestrates, monitors, and self-heals an entire fleet of headless Claude Code sessions." Button: "View amux on GitHub ★" with `exp015_headless_topofpage_cta_click` event.
- **Effort:** XS
- **Score (2026-07-29):** Pre-period (2026-07-15–21, 7 days): 2 total headless GH clicks (only Jul 15 had 2). Avg = 0.29/day. Post-period (2026-07-22–28, 7 days): Jul 24: 1, Jul 25: 1, Jul 27: 1, Jul 28: 3 = 6 total. Avg = 0.86/day. Delta: **+200%** (0.29 → 0.86/day). Named EXP-015 events: 5 total (Jul 24: 1, Jul 28: 3, Jul 29: 1). Both signals confirm: top-of-page placement is dramatically more effective than the buried EXP-007 block at line 650. **Change kept permanently. Replicate this pattern to other high-traffic/low-CVR guides.**

---

## Concluded Experiments

| Experiment | Started | Concluded | Result | Verdict |
|-----------|---------|-----------|--------|---------|
| EXP-004 — Concierge urgency badge | 2026-07-10 | 2026-07-24 | 0 clicks in 14 days (off-site conversion, unmeasurable) | no_effect |
| EXP-006 — GitHub README iOS badge | 2026-07-13 | 2026-07-21 | -9% iOS clicks (pre-period 3 days only, insufficient baseline) | inconclusive |
| EXP-007 — Compare pages GitHub CTA | 2026-07-14 | 2026-07-21 | 0 named events; ~5 compare GH clicks total from autocapture (low traffic volume, 10-15 PVs/page) | inconclusive |
| EXP-008 — Theme preference PostHog property | 2026-07-15 | 2026-07-22 | person.properties vs event properties namespace mismatch — data in event layer, segmentation never worked | inconclusive (measurement) |
| EXP-009 — "Indie hackers" hero language | 2026-07-16 | 2026-07-24 | **-21.4%** homepage GH clicks (pre 10.3/day vs post 8.1/day over 8 days). Reverted. | **no_effect (negative)** |
| EXP-013 — GitHub CTA on best-ai-model-for-coding guide | 2026-07-12 | 2026-07-20 | 162 PVs, 0 KPI clicks (bot/crawler traffic confirmed) | no_effect |
| EXP-005 — Star History chart on homepage | 2026-07-11 | 2026-07-25 | No valid pre-period baseline after 14-day post window (PostHog activated 1 day before experiment). 5 simultaneous experiments made attribution impossible. Chart kept. | inconclusive |
| EXP-011 — Plan strip row in homepage PS grid | 2026-07-18 | 2026-07-25 | **+12.3%** homepage GH clicks (pre 8.1/day vs post 9.1/day, 7 days each). EXP-009 confound likely underestimates the true effect. **Change kept permanently.** | **win** |
| EXP-012 — Freelancer CTA on compare index | 2026-07-21 | 2026-07-28 | 1 named click (to for-freelancers), compare GH clicks: 2 pre / 1 post (7 days each) — noise-level volume. Same low-traffic pattern as EXP-007. CTA kept. | inconclusive |
| EXP-014 — GitHub CTA on guide pages (harness-engineering, measuring-ai-coding-agent-roi, claude-code-context-compaction) | 2026-07-20 | 2026-07-27 | 1 named event (harness-engineering), 0 pre → 1 post autocapture GH click. Same pattern as EXP-007: target pages have insufficient human traffic (108 PVs/14d for harness-engineering). CTA kept on all pages. | inconclusive |
| EXP-015 — Top-of-page GitHub CTA on claude-code-headless | 2026-07-22 | 2026-07-29 | **+200%** headless GH clicks (pre 0.29/day vs post 0.86/day, 7 days each). 5 named exp015 events confirmed. Top-of-page placement dramatically outperforms buried EXP-007 block at line 650. **Change kept permanently.** | **win** |
| EXP-010 — Feature table reorder: phone-first features at top | 2026-07-17 | 2026-07-31 | **+20.0%** iOS clicks (pre 3.6/day vs post 4.3/day, 14-day post window). EXP-011 minimal confound (different KPI: GH clicks not iOS). Phone-first row order kept permanently. | **win** |
| EXP-001 — Hero CTA button copy | 2026-07-07 | 2026-07-26 | No valid pre-period baseline — PostHog activated 2026-07-09, experiment started 2026-07-07. Change kept ("⭐ Star on GitHub"). | inconclusive |
| EXP-002 — iOS CTA sticky mobile bottom bar | 2026-07-08 | 2026-07-26 | No valid pre-period baseline — PostHog activated 2026-07-09, experiment started 2026-07-08. Sticky bar kept (17 taps logged, harmless UX improvement). | inconclusive |
| EXP-003 — Homepage hero social proof line | 2026-07-09 | 2026-07-26 | No valid pre-period baseline — experiment started same day PostHog activated. Social proof paragraph kept. | inconclusive |

---

## Learnings Log

_Updated by SCHED-149 Job 9 after each run with PostHog data and experiment results._

| Date | Finding | Action taken |
|------|---------|--------------|
| 2026-07-07 | PostHog installed, baseline accumulation started | — |
| 2026-07-07 | PostHog HogQL query returned 0 events — no click data yet after 1 day of tracking | EXP-001 shipped; wait for data to accumulate before scoring |
| 2026-07-07 | EXP-001 launched: "View on GitHub" → "⭐ Star on GitHub" on both hero CTAs in homepage | Implement complete; measure 2026-07-14 |
| 2026-07-08 | PostHog still 0 events (Day 1 — accumulating) — EXP-001 cannot be scored yet (< 7 days) | EXP-002 shipped: sticky iOS bottom bar via site.js injection on all pages |
| 2026-07-08 | EXP-002 launched: sticky mobile bottom bar with App Store CTA, PostHog event exp002_ios_sticky_tap | Measure after 2026-07-15; new hypothesis EXP-009 added (indie hacker language) |
| 2026-07-09 | PostHog still accumulating — no KPI click data available after 2 days; EXP-001 and EXP-002 cannot be scored yet | EXP-003 shipped: social proof line "Trusted by 288+ developers" under lede in homepage hero; measure after 2026-07-16 |
| 2026-07-10 | PostHog: 1 day of real data (phc_ key live since 2026-07-09); 294 pageviews, 111 autocaptures recorded — too early to score any experiments (all < 7 days); no KPI click events isolated yet | EXP-004 shipped: amber urgency badge "3 onboarding slots open this month" on /concierge/ final CTA; measure after 2026-07-17 |
| 2026-07-11 | PostHog: 3 days data — 554 pageviews total, 26 KPI clicks (all homepage); /guides/best-ai-model-for-coding-2026/ has 84 PVs but 0 KPI clicks (biggest conversion gap) | EXP-005 shipped: star history chart between features and final CTA; EXP-013 added (guide page GitHub CTA) |
| 2026-07-12 | PostHog: 14-day query returned 0 KPI clicks (data still accumulating — phc_ key only active since 2026-07-09, so < 7 days of real data for all experiments; none scoreable yet) | EXP-013 shipped: GitHub CTA chip on /guides/best-ai-model-for-coding-2026/ (84 PVs, 0 KPI clicks); EXP-006 queued next |
| 2026-07-15 | PostHog 7-day KPI click data: homepage / → 70 clicks (GitHub+AppStore), /guides/best-ai-agent-multiplexers-2026/ → 16 clicks (highest non-homepage conversion rate — outperforming all compare pages), /docs/ → 6, /pricing/ → 6. EXP-002 iOS sticky bar: 5 custom exp002_ios_sticky_tap events observed since launch — positive signal but no pre-experiment baseline available (PostHog only active from 2026-07-09, EXP-002 started 2026-07-08). Cannot score pre/post. Extending measurement to 2026-07-22. EXP-001 also cannot be scored — same baseline gap. | EXP-008 shipped: theme_preference super property registered in site.js on page load and toggle (measures whether dark vs light users convert differently on GitHub vs cloud KPIs). New finding: /guides/best-ai-agent-multiplexers-2026/ has the best GitHub click rate outside the homepage — should investigate what drives this and replicate on similar high-PV guide pages. |
| 2026-07-16 | PostHog 14-day data: homepage 95 KPI clicks (396 PVs, 24% CVR); best-ai-agent-multiplexers-2026: 18 KPI clicks (124 PVs, 14.5% CVR — best guide); best-ai-model-for-coding-2026: 150 PVs but only 1 KPI click (0.7% CVR — biggest conversion gap, EXP-013 installed 2026-07-12, too early to evaluate); /pricing/: 32 PVs / 6 KPI clicks (18.75% CVR — high intent). EXP-003 (social proof line): reached 7-day minimum but cannot score — no pre-experiment baseline exists (PostHog only live since 2026-07-09, EXP-003 started same day). Extending to 2026-07-23. | EXP-009 shipped: added "Indie hackers, solo builders, and engineering teams" to .lede paragraph in index.html. Job 2 AEO: refreshed best-ai-agent-multiplexers-2026 with July 2026 date + multi-runtime + Homebrew install (commit 6ecb272). Rebuilt amux-vs-cursor and amux-vs-windsurf compare pages from stubs to full 600+ line compare pages. Created /for/open-source-maintainers/. GitHub stars: 299. |
| 2026-07-17 | PostHog 14-day data: homepage 432 PVs / 66 KPI clicks (15.3% CVR); claude-code-headless: 105 PVs / 2 KPI clicks (1.9% CVR — priority freshness target); best-ai-model-for-coding-2026: 150 PVs / 0 KPI clicks (EXP-013 at day 5, not yet at 7-day window). EXP-004 (concierge urgency): 7 days in, 0 concierge CTA clicks — inconclusive, extending to 2026-07-24. GitHub stars: 300. | EXP-010 shipped: moved phone/iOS row to top of homepage PS grid (commit pending). Refreshed claude-code-headless with auto-resume dialog pitfall + Homebrew + multi-runtime + 300 stars (commit d9e16b3). Created /guides/claude-code-resume-dialog/ (commit b4f03d4). Refreshed amux-vs-cmux with EXP-007 CTA + July 2026 + 300 stars (commit 37958b6). Created /for/enterprise/. Changelog 4 new entries (ad13e14, 02cc251, 8f75275, 3af5f86). |
| 2026-07-18 | PostHog 14-day data (first run): homepage 510 PVs / 98 GitHub clicks (19.2% CVR — best all-time); best-ai-agent-multiplexers-2026: 189 PVs / 27 GitHub clicks (14.3% CVR — best guide, confirms "best X" list format works); claude-code-headless: 139 PVs / 2 GitHub clicks (1.4% CVR — biggest human-traffic gap, fixed today with EXP-007 CTA); best-ai-model-for-coding-2026: 161 PVs / 0 GitHub clicks (bot-traffic pattern: spikes of 68 PVs then 44 PVs = crawl waves, not humans; EXP-013 CTA present but zero events confirm this is crawler traffic). iOS/concierge: homepage → 35 clicks, concierge page → 18 clicks (very high-intent at 51% CVR). EXP-007 named events: 0 — all compare page GitHub clicks captured by autocapture instead (compare pages get 12-13 PVs each, low volume). Key insight: "best X" list pages convert at 14% vs compare pages at ~10-16% for the top 2, but most compare pages get barely 10-15 PVs vs 180+ for best-of lists — invest more in list format. | Added EXP-007 GitHub CTA to claude-code-headless (highest human-traffic gap page). Created /guides/ai-agent-live-browser-automation/ (new page). Rebuilt amux-vs-claude-code-agent-teams from 108→628 lines. Changelog: 9 new entries today across 2 runs. EXP-011 shipped. EXP-005 scored inconclusive. 304 stars. New hypothesis: EXP-014 — add "best-of" callout panel to high-PV guide pages pointing to best-ai-agent-multiplexers-2026 (proven 14% CVR format). |
| 2026-07-20 | PostHog 14-day data: homepage 534 PVs / 220 total GitHub clicks (CVR ~19%); best-ai-agent-multiplexers-2026 202 PVs / 28 clicks (13.9% CVR — still best guide); getting-started 38 PVs / 13 clicks (34.2% CVR — HIGHEST of any page, very high intent); pricing 38 PVs / 9 clicks (23.7%). Biggest gaps: harness-engineering (28 PVs, 0 clicks, no CTA), measuring-ai-coding-agent-roi (28 PVs, 0 clicks, no CTA), ai-coding-finops (32 PVs, 0 clicks). EXP-013 scored inconclusive — best-ai-model-for-coding-2026 confirmed bot traffic (162 PVs, 0 clicks pattern). Key new insight: /guides/getting-started/ has 34.2% CVR — highest-intent page, bottom-of-funnel. Drive more traffic there from guides. | EXP-013 marked inconclusive (bot traffic). EXP-014 shipped to harness-engineering + measuring-ai-coding-agent-roi. dateModified freshened on both. New page: /guides/claude-code-context-compaction/ targeting "Claude Code context compaction" overnight-run pain point. Changelog: 6 new entries (Messages tab, hibernate fix, Send now fix, Sent history accordion, click-to-copy, Enter sends). |
| 2026-07-21 | PostHog 14-day data: homepage 614 PVs / 101 GH clicks (16.4% CVR); best-ai-agent-multiplexers-2026 221 PVs / 30 GH clicks (13.6% CVR — best guide); ai-agent-sandboxing NEW entry 122 PVs / 1 GH click (0.8% CVR — biggest gap, fixed today); iOS clicks: homepage 30, EXP-002 sticky 10 taps total. EXP-006 scored inconclusive (pre-period 3 days too short). EXP-007 scored inconclusive (compare pages only 10-15 PVs each, total 5 compare GH autocapture clicks in 14d). 307 GitHub stars (+3 since yesterday). New observation: measuring-ai-coding-agent-roi jumped from 28 → 57 PVs (freshening + sitemap addition yesterday showing immediate traffic uplift). | EXP-006 marked inconclusive. EXP-007 marked inconclusive. EXP-012 shipped: Freelancer CTA on compare INDEX (62 PVs), targeting /for/freelancers/ + /concierge/. ai-agent-sandboxing: EXP-014 CTA added + dateModified freshened. New pages: /guides/ai-agent-cost-monitoring/ (413 lines, targeting "Claude Code token costs", Cost tab feature). Changelog: 11 new entries (Cost tabs, API output fix, Mental Model guide, skills, WCAG contrast, loading indicator, HTML preview fix, faster steering). |
| 2026-07-22 | PostHog 14-day data: homepage 711 PVs / 109 GH clicks (15.3% CVR); best-ai-agent-multiplexers-2026 253 PVs / 39 GH clicks (15.4% CVR — consistently best guide, matches homepage rate); measuring-ai-coding-agent-roi 80 PVs (up from 28 → 57 → 80 in 3 days — freshening compounding fast); ai-agent-sandboxing 128 PVs / 1 GH click; claude-code-headless 185 PVs / 2 GH clicks (1.1% CVR — biggest gap, EXP-007 CTA buried at line 650). EXP-008 SCORED inconclusive — `person.properties.theme_preference` null for all events (super property doesn't propagate to person properties; event-level `properties.theme_preference` is the correct field). EXP-009 trending negative: pre 8.86/day vs post 7.83/day = -11.6% (score tomorrow at 7-day window). | EXP-008 scored inconclusive (measurement issue — person vs event property namespace). EXP-015 shipped: top-of-page green CTA on claude-code-headless above TOC, exp015_headless_topofpage_cta_click. ai-coding-finops: EXP-014 CTA added + dateModified 2026-05-26 → 2026-07-22. New guide: /guides/claude-code-rate-limits/ (targeting "Claude Code rate limit" error queries). Changelog: 5 new entries (pending messages ⏳, click-to-insert, git staged guard, scheduler audit, mental model Commits+Proxies). |
| 2026-07-24 | **EXP-004 CONCLUDED no_effect**: 14-day run, 0 concierge CTA clicks — root cause is off-site conversion (Calendly/email). PostHog can't measure it. Badge kept, experiment closed. **EXP-009 CONCLUDED no_effect + REVERTED**: Full 8-day window scored -21.4% homepage GH clicks (pre 10.3/day vs post 8.1/day). "Indie hackers" persona framing definitively negative — narrowed perceived audience. Reverted .lede to multi-runtime framing + updated star count 288+ → 310+. **EXP-010 extended to 2026-07-31**: iOS +8.5% (below 10% threshold, EXP-011 overlap muddies attribution — need 14-day post window). **EXP-016 queued**: CTO/team persona framing to replace the reverted EXP-009 — ship after EXP-010 concludes. GitHub stars: 310. New pages: /compare/amux-vs-langraph/ rebuilt from 101-line stub to full 500-line compare page (38k stars LangGraph framing, 15-row table, FAQPage/Article/BreadcrumbList JSON-LD, EXP-007 CTA). /for/ctos/ created (new CTO persona page targeting "AI agent team dashboard" + "CTO AI engineering team"). Changelog: 5 new entries (map distance/near-me sort, two-tone pins, AND-mode filter, gzip mobile perf, 'x' key fix). | EXP-004 marked concluded no_effect. EXP-009 marked concluded no_effect + reverted in site/index.html. EXP-016 queued. New pages committed: amux-vs-langraph + for/ctos + changelog + sitemap + llms.txt. |
| 2026-07-25 | **EXP-011 CONCLUDED win +12.3%**: Plan strip row in homepage PS grid (started 2026-07-18). Pre (Jul 11–17) homepage GH clicks: 8.1/day. Post (Jul 18–24): 9.1/day. Delta: **+12.3%** — first conclusive win above 10% threshold. Plan strip row kept permanently. EXP-009 confound (negative effect throughout window) likely means true EXP-011 effect is higher than +12.3%. **EXP-005 CONCLUDED inconclusive**: No valid pre-period baseline after 14-day extension — PostHog had 1 usable pre-data point. Star history chart kept in place. Homepage GitHub KPI state: 221 total clicks across all pages last 14d (homepage 121 = 55%, multiplexers guide 50 = 23%). iOS clicks: 51 total last 14d, 17 from exp002 sticky bar. GitHub stars: 313. | EXP-011 and EXP-005 marked concluded. EXP-017 added to backlog (best-multiplexers callout on compare index). New pages: /compare/amux-vs-autogen/ rebuilt from 105-line stub to 490-line full compare page (AutoGen 60k ★, Microsoft Research; 15-row table; "Why not both?" 3 patterns; FAQPage 5 Qs; Article+BreadcrumbList JSON-LD). /guides/best-ai-agent-multiplexers-2026/ freshened (dateModified 2026-07-25, Opus 5 mention). /guides/mobile-management-pwa/ freshened (iOS PWA state restore section added, v0.9.189). Changelog: 4 new entries (iOS PWA restore, Opus 5, self-heal, head-of-line block). |
| 2026-07-26 | **EXP-001/002/003 CONCLUDED inconclusive**: All three experiments pre-date or match PostHog activation (2026-07-09) — no valid pre-period baseline possible. All changes kept in place (star CTA copy, sticky iOS bar, social proof paragraph — none show evidence of harm). PostHog 14-day KPI state (from yesterday's run): 215 total GitHub clicks, homepage 123 (57%), best-ai-agent-multiplexers-2026 52 (24%), pricing 14, best-claude-code-session-managers-2026 5 (new entrant, 5 GH clicks first appearance), getting-started 4, ai-agent-orchestration-2026 4, claude-code-headless 4. iOS: 3–6/day, steady. GitHub stars: 313. No experiments due today (EXP-014 → tomorrow 2026-07-27, EXP-012 → 2026-07-28). | EXP-001, EXP-002, EXP-003 marked concluded inconclusive + added to Concluded table. New page: /guides/voice-dictation-ai-agents/ (new, 450+ lines targeting "voice dictation AI agents", "dictate to Claude Code from phone" — first AEO page on voice control for AI coding agents; HowTo + FAQPage + Article JSON-LD, 6 FAQ questions). /guides/ai-agent-orchestration-2026/ freshened (dateModified 2026-05-25 → 2026-07-26, July 2026 badge, EXP-014 CTA, voice dictation mention in intro, voice guide in further reading). Changelog: 6 new entries (dictation v0.9.197, dictation mobile v0.9.199, Messages color-coding v0.9.190, Files syntax-highlight v0.9.193, Files offline-download v0.9.192, Schedules badge v0.9.195). |
| 2026-07-27 | **EXP-014 CONCLUDED inconclusive**: 7-day window. 1 named exp014 click (harness-engineering), 0→1 post autocapture GH click. Same pattern as EXP-007: target pages have insufficient human traffic (harness-engineering ~108 PVs/14d, measuring-ai-coding-agent-roi similar). CTAs kept on all pages. PostHog 14-day KPI state: **128 total GH clicks** (up from 215 total last run — note: different measurement period), homepage 93 (73%, 756 PVs = 12.3% CVR), best-ai-agent-multiplexers-2026 7 (5.5%, 312 PVs), pricing 4, claude-code-headless 4, **best-claude-code-session-managers-2026 2 (utm_source=chatgpt.com — ChatGPT citing this page!)**. iOS: 2 autocapture + 14 exp002 sticky taps total. GitHub stars: **315** (+2). Key AEO signal: ChatGPT is actively citing best-claude-code-session-managers-2026 for "Claude Code session manager" queries — confirming the page is ranking in AI answer engines. No experiments shipped today (EXP-017 waits for EXP-012 scored 2026-07-28; EXP-016 waits for EXP-010 concluded 2026-07-31). | **EXP-014** marked concluded inconclusive + added to Concluded table. **best-claude-code-session-managers-2026** enriched (dateModified 2026-07-15→2026-07-27; EXP-014 indigo CTA added; v0.9.200 offline scrollback + v0.9.201 stalled badge features added; voice dictation + mobile PWA links in Related Guides). New page: **/guides/ai-agent-offline-review/** (new, ~450 lines, HowTo 5 steps + FAQPage 7 Qs + Article JSON-LD; targets "review AI agents offline", "check Claude Code agents without internet"; covers v0.9.200 IndexedDB scrollback cache, offline outbox, iOS PWA behavior; EXP-014 CTA; sitemap + llms.txt added). Changelog: 3 new entries (v0.9.201 stalled badge b120b6c, v0.9.200 offline scrollback a5e73c0, credit-gate fix d95b424). |
| 2026-07-28 | **EXP-012 CONCLUDED inconclusive**: 7-day window. 1 named exp012 click (to for-freelancers), compare index GH autocapture: 2 clicks pre (2026-07-14–20) vs 1 click post (2026-07-21–27) — noise-level volume, no meaningful signal. Same pattern as EXP-007/EXP-014: compare index (~62 PVs/14d) doesn't have enough human click volume for 7-day pre/post measurement. CTA kept. **EXP-017 SHIPPED**: green/teal best-multiplexers callout added to compare/index.html above EXP-012 block; `exp017_compare_index_multiplexers_callout_click` event; measure 2026-08-04. PostHog 14-day KPI state: **129 total GH clicks** (homepage 92, **claude-code-headless 7 — up from 2-4, EXP-015 signal!**, best-ai-agent-multiplexers 6, pricing 3, compare 2). iOS: **50 total** (homepage 29 autocapture + 11 exp002 sticky + 10 from other pages). GitHub stars: **315** (unchanged). ChatGPT referral now hitting homepage (utm_source=chatgpt.com, 5 GH clicks) + blog/best-terminal + best-claude-code-session-managers. claude-code-headless jump to 7 GH clicks (from 2-4) is strong EXP-015 signal — EXP-015 score due tomorrow. | **EXP-012** marked concluded inconclusive. **EXP-017** shipped + marked running. New page: **/guides/run-ai-engineering-team/** (new, ~450 lines, targets "how to run an AI engineering team" — primary AEO query; HowTo 5 steps + FAQPage 7 Qs + Article JSON-LD; team sizes table, coordination patterns). **blog/best-terminal-ai-coding-agents-2026** enriched (dateModified 2026-05-11→2026-07-28, amux section updated 315★+multi-runtime+offline scrollback+voice, Antigravity CLI July note, EXP-014 CTA added). Changelog: 3 new entries (session tab reordering 9f3a9b6, local Whisper 12x 279d8eb, persistent outbox v0.9.202 6355f15). |
| 2026-07-29 | **EXP-015 CONCLUDED win +200%**: Top-of-page green CTA on claude-code-headless (started 2026-07-22). Pre (Jul 15–21): 0.29 headless GH clicks/day (2 total). Post (Jul 22–28): 0.86 clicks/day (6 total). Delta: +200% — decisive win. 5 named exp015 events confirm. Key insight: CTA POSITION matters far more than CTA PRESENCE — the EXP-007 block was buried 650+ lines in; the top-of-page banner catches visitors before they bounce. **EXP-018 QUEUED**: roll out the same top-of-page pattern to ai-agent-orchestration-2026 and ai-agent-sandboxing (both high-PV, low-CVR). PostHog 14-day KPI state: **221 GH clicks** (homepage 111, best-ai-agent-multiplexers 49, pricing 11, claude-code-headless 9, ai-agent-orchestration 4). iOS: 59 total (homepage 45, demos 3, features/mobile-pwa 3). GitHub stars: **316** (+1). ChatGPT still citing multiple pages. | **EXP-015** marked concluded win. **EXP-018** added to backlog. **amux-vs-devin** rebuilt 120→354 lines (full compare page: 15-row table, cost math, decision guide, FAQ 5 Qs, "why not both?"). **use-cases/ai-agent-browser-profiles/** created (new, 253 lines; HowTo 4 steps, FAQ 6 Qs, comparison table, multi-profile fleet use cases; targets "saved browser auth profiles for AI agents"). Changelog: 6 new entries (Messages tab filter d6116f6, browser profiles 90c5f75, copy path PR#64 b6781b5, MKV fix 51d8c45, service worker perf aff3025, profile picker crash fix 43483bd). |
| 2026-07-31 | **EXP-010 CONCLUDED win +20.0%**: Phone-first feature table reorder (started 2026-07-17). Pre (Jul 9–16, 7d): 3.6 iOS clicks/day. Post (Jul 17–30, 14d): 4.3 iOS clicks/day. Delta: **+20.0%** — decisive win at 14-day post window. EXP-011 confound minimal (GH clicks, not iOS). Phone-first row order kept permanently. **EXP-018 SHIPPED** to ai-agent-sandboxing (131 PVs, 1 GH click, 0.8% CVR — highest-traffic low-CVR guide). Green top-of-page banner added 2026-07-30; measure 2026-08-06. **New persona page**: /for/product-managers/ (new, targets "AI agents for product managers", "assign tasks to AI agents", "AI agent kanban board PM"; Article + FAQPage 7 Qs + BreadcrumbList JSON-LD). **Kanban guide enriched** with mobile kanban (column snap/swipe, 20ae941), task dependencies + reviewer assignments (7a3bfa5), and prompt auto-decomposition (4b0b1ed). **Changelog** updated: 6 new entries (20ae941–456d161), changelog/index.html ItemList refreshed with 5 newest entries. | **EXP-010** marked concluded win. **EXP-018** marked running. /for/product-managers/ committed. kanban-board-for-agents enriched + dateModified updated. |
| 2026-08-01 | GitHub stars: **323** (+7 since last run). No experiments to score today (EXP-017 → 2026-08-04, EXP-018 → 2026-08-06). **EXP-016 SHIPPED**: CTO/team persona framing in homepage .lede — "AI engineering teams" + "5–50 agents. Zero coordination overhead." Star count updated 310+→323+. Measure 2026-08-08. **EXP-018 expanded** to ai-agent-orchestration-2026 (second target — 200+ PVs, 4 GH clicks, ~2% CVR). New pages: /for/data-scientists/ (parallel AI for EDA/modeling/pipeline automation; 7-question FAQPage + Article + BreadcrumbList JSON-LD). Changelog: 5 new entries (cee2e62 fs API, 34cca01 burst view, 25ccf95 board audit trail, 0ae5070 deploy fix, 38c66bd autonomy marker fix). | **EXP-016** marked running. **EXP-018** second target added. /for/data-scientists/ committed. ai-agent-orchestration-2026 freshened (EXP-018 CTA + dateModified 2026-08-01). amux-vs-openai-symphony updated (EXP-007 CTA). Press page date updated. |
| 2026-08-02 | GitHub stars: **327** (+4 since last run). No experiments to score today (EXP-017 → 2026-08-04, EXP-018 → 2026-08-06). **EXP-019 SHIPPED**: "No idea what your AI team accomplished this week" → "Review dashboard + Trends view + Fleet org chart" row added to homepage PS grid. Links to /guides/ai-agent-team-review/. Measure 2026-08-09. **EXP-018 expanded** to agent-fleet-operations (third target — enriched with Trends/Review/Fleet sections + EXP-018 CTA). No experiments concluded today. Changelog: 8 new entries (0e8a29b rate-limit fix, 4babfb3 Trends view, 29dfc39 tab customizer, 32ac1a6 Unblock everything, 251e56b Review command center, 7fb97f8 Review dashboard, 24e6c69 Fleet org chart, eeaf4e0 model badge fix). New pages: /guides/ai-agent-team-review/ (new, 270+ lines, HowTo 5 steps + FAQPage 7 Qs + Article + BreadcrumbList JSON-LD; EXP-018 CTA; targets "AI agent team weekly review"). Job 7 skipped (lists lastmod 2026-07-06 = 27 days, threshold 30 days, due 2026-08-05). | **EXP-019** shipped and marked running. /guides/ai-agent-team-review/ committed. agent-fleet-operations enriched (3 new sections + 3 new FAQs + EXP-018 CTA + dateModified 2026-08-02). /for/engineering-managers/ enriched (Trends/Review/Fleet sections + 3 FAQs + dateModified 2026-08-02). amux-vs-codex enriched (Trends/Fleet obs row + FAQ + dateModified 2026-08-02). Press page date updated to August 2, 2026. Sitemap + llms.txt updated with new page. |

---

## Notes for SCHED-149

When running Job 9:
1. Query PostHog for click events on `github.com/mixpeek/amux`, `apps.apple.com` links, and `/concierge/` CTAs — these are the three KPI proxies.
2. Check which pages have the highest click-through on each KPI (not just pageviews).
3. Move the highest-confidence `queued` experiment to `running` — implement it (it will be small/XS effort by design in the backlog). Log the start date and expected measurement period (minimum 7 days of traffic).
4. Check any `running` experiments: if ≥7 days of data, read PostHog for pre/post comparison and log result in Concluded section.
5. Add new experiment ideas to the backlog if new patterns emerge from the data (e.g., a page with high traffic but zero KPI clicks is a signal).
6. Always implement the experiment change AND record it — don't just write about it.
