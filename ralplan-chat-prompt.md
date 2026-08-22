/ralplan Amux: chat vrstva (core + dashboard + Telegram) pro dev sessions + zavření localhost auth díry na /send

> **Aktualizováno 2026-07-22** — po dvou upstream merge (v0.9.108 → `4a60aa6`, dohromady
> ~620 commitů) a po zavedení fork-governance pravidel (Local Delta Registry, AMUX-LOCAL
> sentinel, dvouúrovňová pre-merge brána). Rozhodnutí Jana z 2026-07-22 jsou zapracovaná
> a NEJSOU už otevřené otázky plánu (viz „Předrozhodnuto" níže).

## Kontext a motivace

Repo: ~/Desktop/Projects/amux — jednosouborový server `amux-server.py` (~54 600 řádků,
Python stdlib ThreadingHTTPServer, port 8822, HTTPS default, inline HTML/JS dashboard).
Sessions = persistentní tmux okna s interaktivním Claude Code CLI (ne `claude -p`).

Strategické rozhodnutí (Jan, 2026-07-21): amux se stává PLATFORMOU PRO DEV AGENTY
(interaktivní, attended/semi-attended práce na mac-brainu). Oddělený systém „agents"
na mac-serveru (Telegram bot + launchd + efemérní claude -p wrapper) zůstává NEDOTČEN
pro autonomní/scheduled/rodinné lany — není součástí tohoto zadání a nesmí se měnit.

Cíl: chatovat s běžícími amux dev sessions — lokálně v dashboardu i vzdáleně přes
Telegram (mobil), nad JEDNOU sdílenou historií, bezpečně.

## Návaznost na existující governance (POVINNÉ pro plán)

Tento fork má od 2026-07-21 závazná pravidla pro in-file změny — plán s nimi musí počítat:

1. **Sidecar-first žebřík** (`.claude/rules/extend-via-sidecar.md`): nová funkcionalita
   patří vedle serveru (sidecar / referenced files), ne dovnitř `amux-server.py`.
   Nevyhnutelné in-file změny (prereq A je taková — `_check_auth` žije v serveru) musí
   ve STEJNÉM commitu: (a) sentinel `# AMUX-LOCAL:<feature>` … `# /AMUX-LOCAL:<feature>`
   (nikdy house-style `# ── … ──`), (b) řádek v **Local Delta Registry**
   (`MODIFICATIONS.md`) s unique-to-local grep landmarky, (c) resolution note v registru,
   (d) posouzení upstreamovatelnosti.
2. **Šablona pro first-class UI feature** (`docs/session-chat.md` — Phase 0 audit,
   NEimplementováno): split do `chat.js`/`chat.css`/`amux_chat.py` + ~20řádkový
   sentinel-footprint + DI `amux_chat.init(ctx)`. Plán ji recykluje, pokud zvolí
   samostatný tab (viz Předrozhodnuto #3).
3. **Týdenní upstream sync** (`docs/upstream-sync.md`, scheduler SCHED-1): každá nová
   in-file delta zvětšuje merge plochu — plán má minimalizovat počet a rozsah in-file
   zásahů a každý registrovat.

## Co se v upstreamu změnilo a plán na tom staví (ověřeno 2026-07-22)

- **Steering výrazně vyzrál**: settle 9→2 s, fast tick, opravy delivery (`ef42dff`,
  `0e440af`), a hlavně **deterministický test harness
  `tests/test_steering_delivery.py`** — načítá reálný `_steer_try_deliver` přes AST
  s fake clockem, testuje settle window, one-per-boundary FIFO, guard folding,
  subagent-hold. B1 doručování MUSÍ jít výhradně přes tento steering a test plán
  MUSÍ rozšířit tento harness (ne psát vlastní paralelní).
- **Upstream má nový „Messages tab"** v dashboardu (`4a60aa6`): per-session historie
  odeslaných zpráv/steerů, offline fronta s ⏳pending kartami, `saved_messages`
  tabulka v SQLite. Je to zatím JEDNOSMĚRNÁ historie příkazů (bez odpovědí session)
  s client-side `cmdHistory`. Překrývá se s B2 — plán rozhodne (Předrozhodnuto #3).
- **Upstream zavedl hook infrastrukturu** (`scripts/git-hooks/pre-commit`,
  `scripts/install-hooks.sh`) — pokud plán potřebuje enforcement, napojit sem,
  nevymýšlet vlastní.

## Předrozhodnuto (Jan, 2026-07-22 — plán NEotevírá znovu)

1. **Prereq A mechanismus: localhost token z 0600 souboru** v `~/.amux/` (ne unix
   socket). Plán navrhne detaily (název souboru, hlavička/parametr, rotace, kdo ho
   čte: CLI, sidecary, inter-session sendy, scheduler, watchdog) — volba mechanismu
   je hotová. Design má být generický (upstreamovatelný — silný PR kandidát pro
   mixpeek/amux; v registru označit Upstreamable=Y).
2. **Fázování: JEDEN plán s fázovanými milníky** A → B1 → B2 → B3, každý milník
   s vlastními AC a verifikací; implementace po jednom schválení, sekvenčně.
3. **B2 UI cesta: rozhodne konsensus** — rozšířit upstreamí Messages tab (menší UI
   práce, ale in-file JS delta v aktivně vyvíjeném místě) vs. samostatný chat tab
   podle session-chat šablony (konflikt-imunní split, ale částečná duplicita).
   Vážit primárně týdenní merge cost.
4. **B3 umístění: rozhodne konsensus** — sidecar proces vs. in-server vlákno; pravidla
   repa dávají sidecar-first prior (in-server vlákno = trvalá in-file delta, musela by
   projít checklistem z governance a mít silné zdůvodnění).

## Rozsah

### A. [BLOKUJÍCÍ PREREQ, milník 1] Localhost auth na write endpointech
Dnes: `_check_auth` (~:45495) bypassuje auth pro localhost (127.0.0.1/::1) kompletně;
`POST /api/sessions/<name>/send` je na localhostu UNGATED — kterýkoli lokální proces
může injektovat text do tool-enabled Claude session (vlastní komentář v kódu ~:53998:
„unauthenticated RCE on YOLO sessions"). CSRF gate chrání jen browser Origin;
curl/node/python projdou. Dashboard má vlastní `X-Amux-UI-Token` mechanismus (~:739).

Požadavek: write endpointy (`/send`, `/steer`, `/wake`, create/config/stop session,
board write, schedules write — přesný výčet určí plán) musí vyžadovat localhost token
(Předrozhodnuto #1) I Z LOCALHOSTU. Nesmí rozbít: dashboard (X-Amux-UI-Token),
existující CLI `amux` (symlink /usr/local/bin/amux → repo), inter-session komunikaci
(sessions si posílají zprávy přes /send — musí dostat token), scheduler (SCHED-1 pondělní
sync posílá do amux-helper!), watchdog, sidecary. Read-only endpointy
(peek/health/events) mohou zůstat volnější — plán rozhodne a zdůvodní.
Změna je in-file → sentinel + registry řádek + resolution note (governance výše).

### B. Chat vrstva (nová funkce) — core + dva klienti

**B1. Chat core (milník 2):** per-session vlákno zpráv v SQLite (owner zprávy,
odpovědi session, systémové události — limit, restart, chyba). Vstup: zpráva
do vlákna → doručení do session VÝHRADNĚ přes existující steering (turn boundary,
settle window, durable, delivered-flag — sémantika z `tests/test_steering_delivery.py`).
Výstup: dokončený tah session (session_idle/transcript) → záznam odpovědi do vlákna
→ push všem připojeným klientům. Každá zpráva má idempotentní ID + origin tag
(dashboard/telegram/system) — zrcadlení mezi klienty nesmí způsobit druhé doručení
do session (echo-smyčka). Plán navrhne schéma tabulky a vztah ke
steering_queue/steering_history I k upstreamí `saved_messages`/cmdHistory
(nezdvojovat stav; rozhodnout, zda odpovědi session doplní existující Messages
model, nebo vznikne nová tabulka).

**B2. Dashboard klient (milník 3):** chat na úrovni tahů (peek zůstává pro syrový
terminál/tool-approval). UI cesta dle Předrozhodnuto #3. Live updates přes existující
SSE (`/api/events`) — POZOR na pravidlo `.claude/rules/sse-realtime.md`: nový datový
zdroj v SSE musí přibýt i do polling fallbacku. Write přes tentýž auth mechanismus
jako prereq A (chat nesmí obejít novou write auth).

**B3. Telegram klient (milník 4):** VLASTNÍ bot token (oddělený od bota „agents"
systému), long-polling (CGNAT, žádný inbound). Owner-only (user_id allowlist; cizí
zprávy ignorovat+log). VLASTNÍ dedikovaná forum skupina (NE sdílená se skupinou
agents bota — samostatný bot, samostatná skupina; žádné sdílení topiců ani
cross-machine credential). Mapování: forum topic ↔ session. Zprávy a odpovědi jen zrcadlí
chat core (B1) — konektor sám nedrží žádnou vlastní historii ani delivery logiku.
Příkazy (minimálně): výpis sessions + stav (idle/active/waiting/limit), peek
N řádků, create/wake session, mute topicu. Token + owner user_id v konfigu/env,
soubor 0600, nikdy v gitu (vzor: `~/.amux/server.env`). Umístění dle
Předrozhodnuto #4 (sidecar-first prior).

**Stavy povrchovat v OBOU klientech, ne tiše polykat:** usage-limit
(wait-for-reset) → zpráva do vlákna; session neexistuje/archivovaná →
srozumitelná chyba; restart amuxu → klienti se zotaví (server se re-execuje na
mtime změnu — launchd KeepAlive vzor existuje), durable fronta nic neztratí.

## Invarianty (nesmí se porušit)
1. Bot token a jakýkoli write credential žijí POUZE na mac-brainu (0600).
2. Steering sémantika: žádné dvojí doručení téže zprávy (i po rebootu/rewake —
   delivered-flag), doručení jen na turn boundary/settle, nepřerušovat běžící tah
   ani tool-approval dialog. `tests/test_steering_delivery.py` musí dál procházet.
3. 8822 se nevystavuje veřejně; Tailscale-only pro remote dashboard zůstává.
4. Žádné zásahy do ~/Desktop/Projects/agents ani do mac-server systému.
5. Nezavádět autonomní orchestrator+worker smyčku — mimo scope tohoto kola
   (board/steering primitiva stačí, smyčkou je člověk).
6. Governance forku: každá in-file delta = sentinel + registry řádek ve stejném
   commitu; commit messages jednořádkové bez trailerů; po každé změně
   `amux-server.py` ověřit `ast.parse` + health 200 (server se re-execuje živě).

## Známé opěrné body v kódu (ověřeno 2026-07-22 na `e9447d4`; před použitím re-grep)
`_check_auth` :45495 (localhost bypass), `_classify_request` :2099 (routing),
`_steer_enqueue` :5214 / `_steer_try_deliver` :5302, `_steering_queue` :2177,
UI token check :739, „unauthenticated RCE" komentář :53998, `saved_messages`
schéma :6369, session .env ~/.amux/sessions/, SQLite ~/.amux/amux.db
(steering_queue, steering_history, saved_messages), SSE /api/events,
steering testy tests/test_steering_delivery.py, watchdog scripts/watchdog.py,
Local Delta Registry MODIFICATIONS.md, sync SOP docs/upstream-sync.md.

## Akceptační kritéria (minimum; plán rozšíří)
- AC1: lokální proces BEZ tokenu → /send|/steer odmítnuto; s tokenem → OK;
  dashboard, CLI, inter-session send, scheduler (vč. pondělního SCHED-1!) fungují
  beze změny chování.
- AC2: owner zpráva v Telegram topicu → doručena do běžící session na turn
  boundary → odpověď se objeví v témže topicu. Ne-owner → nic.
- AC3: simulovaný reboot s nedoručeným steerem → po rewake právě jedno doručení
  (rozšíření harness testu, ne ruční test).
- AC4: session na usage-limitu → notifikace do vlákna/topicu (žádný tichý stall).
- AC5: pád/restart amuxu → konektor i chat se samy zotaví, žádné ztracené owner
  zprávy (durable fronta).
- AC6: regresní — existující sessions/dashboard/schedules/board beze změny chování;
  `tests/test_steering_delivery.py` prochází beze změn sémantiky.
- AC7: zpráva poslaná z dashboardu se zobrazí v Telegram topicu a naopak;
  do session se doručí PRÁVĚ JEDNOU (origin tag + idempotentní ID — echo test).
- AC8: chat historie je konzistentní napříč klienty po restartu serveru
  i po rebootu stroje (čte se z SQLite, ne z paměti klientů).
- AC9: každá in-file delta z tohoto projektu má sentinel + řádek v Local Delta
  Registry; následný `docs/upstream-sync.md` gate (Tier 1) projde nad novými
  landmarky.

## Očekávaný výstup plánu
Konsensuální plán (deliberate mode) s: designem localhost tokenu (soubor, hlavička,
distribuce konzumentům, rotace, upstreamovatelnost — mechanismus je předrozhodnutý),
schématem chat store a vztahem ke steering + saved_messages tabulkám (B1),
rozhodnutím Messages-tab-vs-samostatný-tab (B2) a in-server-vs-sidecar (B3)
s odůvodněním přes týdenní merge cost, přesným výčtem endpointů pro auth,
pre-mortem (min. 3 scénáře: injekce obejde auth; dvojí doručení / echo-smyčka;
konektor spadne a zprávy se ztratí), test plánem (unit/integrace/e2e/observabilita —
postaveno na test_steering_delivery.py harness) a fázovanými milníky A→B1→B2→B3
s AC per milník. Implementace až po schválení.
