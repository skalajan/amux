#!/usr/bin/env bash
# Re-points the iOS app at the owner's Apple team (upstream signs with Ethan's team and fastlane match).
# Idempotent; run after every upstream merge. Exits 1 if an upstream identifier survives.
set -euo pipefail
cd "$(dirname "$0")/.."

TEAM=8W7PPJLYL3
BUNDLE=cz.janskala.amux

files=(ios/project.yml)
while IFS= read -r f; do files+=("$f"); done < <(
  grep -rl --include='*.swift' --include='*.entitlements' -e 'com\.EthanSteininger\.nextup' ios || true)

sed -i '' \
  -e "s/com\.EthanSteininger\.nextup/${BUNDLE}/g" \
  -e "s/DEVELOPMENT_TEAM: \"M225B6YA42\"/DEVELOPMENT_TEAM: \"${TEAM}\"/" \
  -e 's/CODE_SIGN_STYLE: Manual/CODE_SIGN_STYLE: Automatic/' \
  -e 's/CODE_SIGN_IDENTITY: "Apple Distribution"/CODE_SIGN_IDENTITY: "Apple Development"/' \
  -e '/PROVISIONING_PROFILE_SPECIFIER: "match AppStore/d' \
  "${files[@]}"

left=$(grep -rn --include='*.swift' --include='*.entitlements' --include='project.yml' \
  -e EthanSteininger -e M225B6YA42 -e 'match AppStore' -e 'CODE_SIGN_STYLE: Manual' ios || true)
if [ -n "$left" ]; then
  echo "ios-signing: upstream identifiers remain:" >&2
  echo "$left" >&2
  exit 1
fi
echo "ios-signing: ok (team ${TEAM}, bundle ${BUNDLE})"
