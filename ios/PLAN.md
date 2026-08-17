# amux iOS App — Plan

## Approach

WKWebView native shell. The iOS app is a thin Swift wrapper around the existing amux web dashboard — no duplicate UI, no React Native, no separate codebase. `amux-server.py` is unchanged.

## Architecture

```
ios/
  AmuxApp/
    Sources/
      AmuxApp.swift           # @main app entry point
      ContentView.swift       # WKWebView + pull-to-refresh + toolbar
      ServerManager.swift     # UserDefaults-backed URL config
      ServerPickerView.swift  # first-launch onboarding
      WebView.swift           # UIViewRepresentable WKWebView wrapper
    Assets.xcassets/          # icons, colors
  project.yml                 # XcodeGen spec (source of truth)
  Gemfile                     # fastlane dependency
  fastlane/
    Fastfile                  # lanes: test, beta, release
    Matchfile                 # cert management via git repo
    Appfile                   # bundle ID, Apple ID
```

## Server Connection

Two modes, configured on first launch:
- **Cloud**: `https://cloud.amux.io` — Clerk auth via WKWebView cookie store
- **Local**: any user-supplied URL (e.g. `https://your-machine.tail-xxxx.ts.net:8824`)
  - Self-signed cert accepted via `URLAuthenticationChallenge` delegate

Stored in `UserDefaults`. Switchable from Settings (gear icon in toolbar).

## Features

- Persistent login (WKWebView cookie store survives app restarts)
- Pull-to-refresh
- Loading indicator
- Swipe back/forward
- Self-signed cert support (Tailscale local installs)
- Server switcher (toolbar gear → switch between saved servers)

## CI/CD — GitHub Actions → App Store

Workflow: `.github/workflows/ios.yml`
- Triggers on push to `main` when `ios/**` changes, or `workflow_dispatch`
- Runs on `macos-latest`
- Fastlane `match` pulls certs from a private git repo (encrypted)
- Fastlane `release` lane: bump build number → build → upload to TestFlight
- Promote TestFlight → App Store manually (or auto via `deliver`)

### Build number strategy
`CFBundleVersion` = git commit count (`git rev-list --count HEAD`)
`CFBundleShortVersionString` = `VERSION` file in repo root (manually bumped for marketing versions)

### Required secrets (one-time setup)
| Secret | Description |
|---|---|
| `MATCH_PASSWORD` | Fastlane match passphrase |
| `MATCH_GIT_URL` | Private repo with encrypted certs |
| `ASC_KEY_ID` | App Store Connect API key ID |
| `ASC_ISSUER_ID` | App Store Connect issuer ID |
| `ASC_KEY_CONTENT` | `.p8` key content (base64) |

## Implementation steps

- [x] 1. Scaffold `ios/` — project.yml, Swift sources, Fastlane config
- [ ] 2. Generate `.xcodeproj` with xcodegen, verify it opens in Xcode
- [ ] 3. Register bundle ID `io.amux.app` in App Store Connect
- [ ] 4. Set up fastlane match (create private certs repo, run `match init`)
- [ ] 5. Add GitHub workflow + secrets
- [ ] 6. First TestFlight build via CI
- [ ] 7. App Store submission (screenshots, description, review)
