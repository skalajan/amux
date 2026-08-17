#!/usr/bin/env bash
# One-time Google OAuth setup — writes credentials to ~/.amux/server.env
set -e

CREDENTIALS_FILE="${1:-}"
SERVER_ENV="$HOME/.amux/server.env"

if [ -z "$CREDENTIALS_FILE" ]; then
  echo "Usage: $0 /path/to/google-credentials.json"
  echo ""
  echo "Get credentials from:"
  echo "  https://console.cloud.google.com/ → APIs & Services → Credentials → Create OAuth 2.0 Client ID (Desktop App)"
  exit 1
fi

if [ ! -f "$CREDENTIALS_FILE" ]; then
  echo "Error: credentials file not found: $CREDENTIALS_FILE"
  exit 1
fi

echo "Reading credentials from $CREDENTIALS_FILE..."

CLIENT_ID=$(python3 -c "import json,sys; d=json.load(open('$CREDENTIALS_FILE')); print(d.get('installed',d.get('web',{})).get('client_id',''))")
CLIENT_SECRET=$(python3 -c "import json,sys; d=json.load(open('$CREDENTIALS_FILE')); print(d.get('installed',d.get('web',{})).get('client_secret',''))")

if [ -z "$CLIENT_ID" ] || [ -z "$CLIENT_SECRET" ]; then
  echo "Error: could not parse client_id/client_secret from credentials file"
  exit 1
fi

echo "Client ID: ${CLIENT_ID:0:20}..."
echo ""
echo "Opening browser for OAuth consent..."

SCOPE="https://www.googleapis.com/auth/drive%20https://www.googleapis.com/auth/documents"
REDIRECT="urn:ietf:wg:oauth:2.0:oob"
AUTH_URL="https://accounts.google.com/o/oauth2/auth?client_id=${CLIENT_ID}&redirect_uri=${REDIRECT}&scope=${SCOPE}&response_type=code&access_type=offline"

open "$AUTH_URL" 2>/dev/null || xdg-open "$AUTH_URL" 2>/dev/null || echo "Open this URL in your browser: $AUTH_URL"

echo ""
read -p "Paste the authorization code here: " AUTH_CODE

echo "Exchanging code for refresh token..."

RESPONSE=$(curl -s -X POST https://oauth2.googleapis.com/token \
  -d "code=${AUTH_CODE}" \
  -d "client_id=${CLIENT_ID}" \
  -d "client_secret=${CLIENT_SECRET}" \
  -d "redirect_uri=${REDIRECT}" \
  -d "grant_type=authorization_code")

REFRESH_TOKEN=$(echo "$RESPONSE" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('refresh_token',''))")

if [ -z "$REFRESH_TOKEN" ]; then
  echo "Error: failed to get refresh token. Response:"
  echo "$RESPONSE"
  exit 1
fi

mkdir -p "$(dirname "$SERVER_ENV")"
touch "$SERVER_ENV"

# Remove any existing gdrive entries
sed -i '' '/^GDRIVE_/d' "$SERVER_ENV" 2>/dev/null || sed -i '/^GDRIVE_/d' "$SERVER_ENV"

# Append new credentials
cat >> "$SERVER_ENV" << EOF
GDRIVE_CLIENT_ID=${CLIENT_ID}
GDRIVE_CLIENT_SECRET=${CLIENT_SECRET}
GDRIVE_REFRESH_TOKEN=${REFRESH_TOKEN}
EOF

echo ""
echo "Credentials written to $SERVER_ENV"
echo ""
echo "Next: restart the server to reload (launchctl kickstart -k gui/\$(id -u)/com.amux.server-rs), then start a new session."
