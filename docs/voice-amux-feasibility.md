# Voice-driven amux — feasibility (AMUX-2980)

Ethan 09:16 ("dope AM idea"): a Bluetooth audio recorder that syncs to the amux
app; use the iOS native integration (amux is in the App Store); every voice
entry point runs onboard STT then an LLM that knows the whole amux API and all
your workers + their descriptions + groups.

This is a research/feasibility pass grounded in what already exists.

## What already exists (the foundation, so this is not from scratch)

- **A native iOS app.** `ios/AmuxApp.xcodeproj` — SwiftUI, ~710 lines, App-Store
  published (docs/app-store-listing.md). It is a WKWebView shell hosting the
  dashboard and connecting to your amux server (Wi-Fi / Tailscale / cloud
  tunnel). A PWA path exists too.
- **A web↔native bridge, already wired.** `WebView.swift` registers a
  `WKScriptMessageHandler` + `WKUserContentController` and calls back with
  `evaluateJavaScript` (today it ships console logs native→web). So a native
  capability (Speech, audio, Bluetooth) reaches the web UI by registering one
  more message handler and evaluating JS — an extension of a channel that
  works, not a new bridge.
- **Voice→text, server-side.** `api/dictation.rs` runs a warm openai-whisper
  worker with a Gemini fallback (`/api/dictation`, `/api/tts` sibling). It
  transcribes; it does NOT interpret intent or route anywhere.

## The three parts, by feasibility and value

### Part 1 — the amux-aware LLM router (BUILD FIRST: server-side, no hardware/native, highest value)
The "LLM that knows all your workers, descriptions, groups, and the API" is the
brain that makes voice *useful*, and it is independent of BT/native. It layers
on the EXISTING voice→text: transcript + live fleet context (workers +
descriptions + groups + the relevant API surface) → a STRUCTURED amux action
(send to worker X, ask group Y, create/append a board card, run a verb), which
the server executes.

- Ethos-aligned: composes dictation + the fleet/board/message primitives; it is
  a router over primitives, not a new primitive.
- Works TODAY with the dashboard mic / PWA dictation — no App Store round trip.
- The load-bearing risk is SIDE EFFECTS: a mis-routed "tell backend to redeploy"
  is a real send. This must reuse the existing confirm-before-act / gate
  discipline — voice proposes the action and the human confirms the
  side-effectful ones (same rule the send path and board gates already enforce).
  Read-only intents ("what is backend doing") can run unconfirmed.

### Part 2 — onboard (on-device) STT in the native app (native, medium effort)
iOS `SFSpeechRecognizer` in on-device mode, in AmuxApp, bridged to the web UI
via the existing `WKScriptMessageHandler` (register a `dictate` handler,
`evaluateJavaScript` the transcript back). Gives fast, offline, no-audio-round-
trip transcription at every voice entry point, feeding Part 1.

- Cost: Swift work + microphone/speech-recognition permissions (standard App
  Store review). The web/PWA keeps the server-side whisper path; native users
  get the faster local one.
- This is what "onboard STT" specifically requires the NATIVE app for — a PWA
  cannot do reliable on-device STT.

### Part 3 — the Bluetooth recorder (hardware, most speculative — split it)
"BT audio recorder" is ambiguous and the two readings have very different cost:
- **(a) Any BT mic/headset** is ALREADY an iOS audio input. Once Part 2 exists,
  a BT mic feeding on-device STT is essentially free — no new code.
- **(b) A dedicated wearable that batches clips and syncs** needs Core Bluetooth
  + that device's own protocol + a decision to ship/endorse a physical device.
  That is a product/hardware bet, not a platform feature.
- Recommendation: (a) rides Part 2 at no cost; treat (b) as a separate product
  decision, not a prerequisite for voice-driven amux.

## Recommended sequence
1. **Part 1** (amux-aware router) — independent, highest value, buildable now on
   dictation.rs + fleet context, gated by the existing confirm-before-act rules.
   This is where "voice knows all your workers" actually lands.
2. **Part 2** (native on-device STT) — improves input latency/offline; the
   native app + its JS bridge already exist to host it.
3. **Part 3(a)** rides Part 2 for free; **Part 3(b)** is a product decision.

Nothing here is blocked on missing infrastructure — the native app, the JS
bridge, and voice→text all exist. The genuinely-new work is Part 1's router
(server) and Part 2's Speech bridge (Swift). Part 1 should be its own card when
prioritized; it is the piece that delivers the idea's value with the least
surface.
