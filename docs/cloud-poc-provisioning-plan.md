# Cloud POC Provisioning: Audit, Gaps, and Build Plan

Date: 2026-07-29. Audited: `cloud/gateway/gateway.py` (2592 lines), `amux-server.py` (attribution + token ledger), `cloud/docker/docker-compose.template.yml`, live prod probes against cloud.amux.io.

## Goal

Provision an isolated amux cloud environment on behalf of a prospect by email. They receive an email invitation, create an account, and land in a dedicated workspace. The POC has a 7-day expiration and a $5 Claude Code budget. Exceeding either shows an upgrade path (Stripe: platform fee + subscription). Ethan has god mode: create accounts, access any workspace, act inside it, with per-user attribution and real-time bidirectional visibility.

## What already exists (verified)

| Capability | Status | Evidence |
|---|---|---|
| Per-user isolated containers | WORKS | Compose stack per user (`amux-user-<id>`), loopback-only ports, Litestream to R2, daily E2E green |
| Clerk auth + signup | WORKS | JWT verify + HMAC cookie; E2E exercises it daily |
| 7-day trial expiration | WORKS | `TRIAL_DAYS=7`, `orgs.trial_ends_at`, hard gate at gateway serving upgrade page / 402 (verified in prod: `in_trial: true`, `trial_ends_at` set on fresh user) |
| Stripe subscription checkout | WORKS | `stripe_configured: true`, `has_annual: true` in prod; webhook flips plan pro/free |
| Shared workspaces (orgs) | WORKS | `orgs`, `org_memberships` (owner/admin/member), `org_invites` (7-day link tokens), invite accept page, `amux_org` cookie switching |
| Same-container multiplayer | WORKS | All members of an org proxy to the same container; SSE broadcasts to every connected client (~2s) |
| Per-turn cost metering | WORKS (in container) | `token_ledger` table prices every Claude Code turn (`cost_usd`); `GET /api/observability?days=N` returns `total_cost` |
| Attribution plumbing | PARTIAL | Gateway injects `X-Amux-User-Email`; server records it on board creator, steer/send history, `/api/identity`; `window._AMUX_USER_EMAIL` in dashboard |
| Admin endpoints | PARTIAL | `ADMIN_EMAILS` gate: logs, container list, user list, read-only SQL, promo codes, cleanup. Admins bypass the trial gate |
| Org API key injection | WORKS | `orgs.api_key` pushed into container `server.env` (our key powers the POC; this is what the budget must protect) |
| Clerk email invitations | AVAILABLE, UNUSED | Clerk `POST /v1/invitations` API confirmed reachable; nothing calls it |

## Gaps

1. **No provision-by-email.** No endpoint creates a workspace for someone who has not signed up. `org_invites` are link-tokens shown in the UI; no email is ever sent.
2. **No admin god mode into workspaces.** Admin endpoints are read-only ops. Ethan cannot open or act inside another user's workspace unless he is an org member. No impersonation or auto-membership.
3. **No budget enforcement.** Cost data exists per container but the gateway never reads it. No budget column, no gate, no shutdown of running workers when the cap is hit (a running agent keeps burning tokens even if the user never loads the page again).
4. **No budget-exceeded modal / platform-fee checkout.** The upgrade page covers trial expiry only, subscription-only pricing. No one-time platform fee line item, no POC-specific copy (production-grade workers, dedicated machine, support, workflow buildout).
5. **Attribution not universal.** Board creator and message history record the acting email, but other mutations (worker create/stop, notes, schedules, files) do not consistently record which human did it. No visible "who did this" in most UI surfaces, no presence indicator.
6. **Admin expiry control.** `trial_ends_at` is set at signup; no admin API to set a custom expiration when provisioning.

## Plan

Each phase: build, test locally + extend `cloud/tests/e2e_smoke.py` (or a new `e2e_poc.py`), deploy, verify in prod, mark the board issue `verified` only with prod evidence.

### Phase 1: Provision-by-email + admin god mode (gateway only)

Build:
- `POST /api/gateway/admin/provision` (ADMIN_EMAILS only): body `{email, expires_days=7, budget_usd=5}`. Creates an org (`org_poc_<hex>`), allocates port, sets `trial_ends_at`, stores `budget_usd`, adds every ADMIN_EMAILS user as `admin` member, creates an `org_invites` token bound to the email, then calls Clerk `POST /v1/invitations` with `redirect_url=https://cloud.amux.io/invite/<token>`. Clerk sends the branded email; signup lands on the existing invite-accept page which already sets `amux_org` and joins them to the org.
- Bind invite acceptance to the invited email (reject if the signed-up email differs) so a forwarded invite cannot hijack the workspace.
- `GET /api/gateway/admin/orgs` list with plan, expiry, budget, spend, members; `PATCH /api/gateway/admin/orgs/<id>` to change expiry/budget.
- Admin workspace switcher: allow `ADMIN_EMAILS` users to pass the membership check in `_active_org_id()` for any org (or simply rely on provision-time auto-membership; auto-membership is simpler and keeps the audit trail honest).

Test: unit-less gateway, so test in prod-like: provision a test email (mailbox we control), assert Clerk invitation created, invite accept joins the right org, admin sees and can enter the workspace.

Verify in prod: provision a real throwaway email end to end, both Ethan and the invited account in the same workspace, then admin cleanup.

### Phase 2: Budget metering + enforcement

Build:
- `orgs.budget_usd` (nullable; null = no cap) + `orgs.spend_cached_usd` + `orgs.spend_checked_at`.
- Gateway poller (reuse the reaper loop cadence, every 5 min): for each running container with a budget, `GET 127.0.0.1:<port>/api/observability?days=30`, cache `total_cost`.
- Enforcement on breach: stop all workers in the container (`POST /api/sessions/<n>/stop` for each), then gate requests exactly like the trial gate: HTML gets the upgrade page, API gets `402 {"error": "budget_exceeded", "spend": X, "budget": Y}`. Admins bypass.
- Important: enforcement must not depend on the user visiting; the poller acts alone.

Test: provision POC org with `budget_usd=0.01`, run one real Claude turn, wait for poller, assert workers stopped and 402 served. Add to `e2e_poc.py`.

Verify in prod: same flow against cloud.amux.io with a real container.

### Phase 3: Upgrade modal + Stripe platform fee

Build:
- New env: `STRIPE_PLATFORM_FEE_PRICE_ID` (one-time). Checkout gains `poc_upgrade` mode: `line_items=[{platform fee, one-time}, {subscription price}]` in subscription mode.
- Budget/expiry upgrade page variant: copy sells the production tier (workers hardened for production, dedicated isolated machine, support + maintenance, ongoing workflow buildout and teaching). Served by the gateway on gate; the dashboard also shows a modal when any API call returns 402 with `budget_exceeded` (client change is generic 402 handling, honoring the single-codebase rule: behavior driven by gateway response, not env branching).
- Webhook: `checkout.session.completed` for a POC org flips plan to `pro`, clears the budget gate.

Test: Stripe test-mode checkout with both line items; webhook flips the org; gate lifts.

Verify in prod: real checkout worker created (can cancel before paying), confirm line items and redirect; webhook verified via Stripe CLI replay or a $0 coupon test.

### Phase 4: Attribution + multiplayer polish

Build (amux-server.py, single codebase):
- Record `X-Amux-User-Email` on every mutating endpoint that currently skips it: worker create/stop/send, notes writes, schedule mutations, file ops. Store in the existing audit/history structures (board already does this).
- Surface it: board card creator chip already exists; add actor email to worker history entries and the activity feed so "who did this" is visible to both parties in real time.
- Presence (optional, if time allows): SSE-driven "Ethan is viewing" indicator using the identity already injected into the page.

Test: two browsers (Ethan + POC user) against one workspace; actions from each side appear on the other within ~2s with correct attribution. Extend `e2e_poc.py` to assert attribution fields via API.

Verify in prod: live two-account worker in the provisioned workspace.

### Rollout order and dependencies

1 → 2 → 3 can ship independently behind admin-only surface area; 4 is server-side and rides the normal deploy. The E2E suite gains `e2e_poc.py` run on the same daily schedule (SCHED-161 companion) so provisioning, budget gate, and attribution stay verified continuously.

## Open items needing Ethan

- `ADMIN_EMAILS` value in `/etc/amux/gateway.env` could not be verified remotely (SSH unreachable from this worker; Tailscale needs re-auth). Confirm it contains your email before Phase 1 verification.
- Stripe: create the one-time platform fee price and pick the amount; confirm POC subscription price ($20/mo current Pro, or a distinct POC-upgrade price).
- Clerk invitation email template: default Clerk branding or custom?
