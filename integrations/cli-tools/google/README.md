# Google Cloud CLI (`gcloud`)

Gives amux sessions access to all Google Cloud services — GCS, BigQuery, GKE, Pub/Sub, Cloud Run, etc.

## Install

```bash
brew install --cask google-cloud-sdk
```

Or download from https://cloud.google.com/sdk/docs/install

Verify:
```bash
gcloud version
```

## Authenticate

### Interactive auth (one-time, persists in `~/.config/gcloud/`)

```bash
# Browser-based login — authorizes gcloud commands
gcloud auth login

# Application Default Credentials — used by SDKs and client libraries
gcloud auth application-default login
```

Both commands open a browser tab and store credentials locally. They persist across shell sessions.

### Service account (for automation / non-interactive)

1. Create a service account in [Google Cloud Console](https://console.cloud.google.com/) → IAM & Admin → Service Accounts
2. Download the JSON key file
3. Store the path in `~/.amux/server.env`:

```bash
echo 'GOOGLE_APPLICATION_CREDENTIALS=/path/to/service-account.json' >> ~/.amux/server.env
launchctl kickstart -k gui/$(id -u)/com.amux.server-rs   # restart the server to reload
```

The `GOOGLE_APPLICATION_CREDENTIALS` env var is the standard way all Google SDKs pick up service account credentials.

## Set a default project

```bash
gcloud config set project YOUR_PROJECT_ID
```

This persists in `~/.config/gcloud/` — no need to pass `--project` on every command.

## Common usage in sessions

```bash
# List GCS buckets
gcloud storage ls

# Copy file to GCS
gcloud storage cp ./file.txt gs://my-bucket/

# List BigQuery datasets
bq ls

# Run a BigQuery query
bq query --nouse_legacy_sql 'SELECT * FROM dataset.table LIMIT 10'

# List Cloud Run services
gcloud run services list --region us-central1

# Tail Cloud Logging
gcloud logging tail "resource.type=cloud_run_revision"

# SSH into a GCE instance
gcloud compute ssh instance-name --zone us-central1-a
```

## MCP alternative

For structured read/write access to Google Docs and Drive from Claude sessions, see the [Google Docs MCP integration](../../google-docs/).

The MCP integration is better for:
- Reading/writing Docs from within Claude's context
- Searching Drive
- Structured data access

The `gcloud` CLI is better for:
- GCS, BigQuery, GKE, Cloud Run, and other GCP services
- Shell scripting and automation
- One-off administrative tasks
