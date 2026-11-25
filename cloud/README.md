# Examail Cloud (Relay)

The service that does what Examail can only do while it's open when the Mac is closed: bring
back snoozed threads, send scheduled mail, keep each account's settings in step across Macs, and
act on arriving mail (muted threads, if-no-reply reminders, split labels, phone notifications).
It also serves the mail to AI agents over MCP. The code is `crates/examail-cloud`; one binary
runs as all three Cloud Run services:

- **`relay-api`**, behind IAP: what the app and the browser call. IAP admits only the Google
  accounts given `roles/iap.httpsResourceAccessor`, and the service checks IAP's signed assertion
  on every request.
- **`relay-worker`**, Cloud Run IAM only: Cloud Scheduler's tick and Gmail's Pub/Sub push.
- **`relay-mcp`**, open to the internet: the MCP server. Every request needs a token the app
  minted (⌘K → Create Remote MCP Token); only tokens' hashes are kept. It searches, reads and
  saves drafts, and never sends.

## Deploying

`cloud/deploy` builds the image with Cloud Build and deploys both services. It reads the target
from `cloud/relay.env`, which git ignores so the project and its URLs stay out of this repo:

```sh
PROJECT=my-project
REGION=us-west1
```

`cloud/deploy --no-build` redeploys the last image, for settings changes.

## One-time setup

In the project, with `P` and `R` the project and region:

```sh
gcloud services enable run.googleapis.com iap.googleapis.com firestore.googleapis.com \
    cloudkms.googleapis.com cloudbuild.googleapis.com artifactregistry.googleapis.com \
    cloudscheduler.googleapis.com secretmanager.googleapis.com --project $P

gcloud firestore databases create --database relay --location $R --type firestore-native \
    --delete-protection --project $P
gcloud kms keyrings create relay --location $R --project $P
gcloud kms keys create refresh-tokens --keyring relay --location $R --purpose encryption \
    --rotation-period 90d --next-rotation-time <90 days from now> --project $P
gcloud artifacts repositories create relay --repository-format docker --location $R --project $P

for s in relay-api relay-worker; do
    gcloud iam service-accounts create $s --project $P
    gcloud projects add-iam-policy-binding $P --role roles/datastore.user \
        --member serviceAccount:$s@$P.iam.gserviceaccount.com --condition None
    gcloud kms keys add-iam-policy-binding refresh-tokens --keyring relay --location $R \
        --project $P --role roles/cloudkms.cryptoKeyEncrypterDecrypter \
        --member serviceAccount:$s@$P.iam.gserviceaccount.com
done
```

The service's Google consent runs through a **Web** OAuth client (made in the console; the API
for making them is gone) whose redirect URI is `<relay-api URL>/v1/connect/callback`. Its id
and secret go in Secret Manager as `relay-oauth-client-id` and `relay-oauth-client-secret`, which
both service accounts may read (`roles/secretmanager.secretAccessor`). Delete the downloaded
JSON.

After the first deploy, open `relay-api` to its users through IAP:

```sh
N=$(gcloud projects describe $P --format 'value(projectNumber)')
gcloud run services add-iam-policy-binding relay-api --project $P --region $R \
    --member serviceAccount:service-$N@gcp-sa-iap.iam.gserviceaccount.com --role roles/run.invoker
gcloud iap web add-iam-policy-binding --project $P --resource-type cloud-run \
    --service relay-api --region $R --member user:<you> --role roles/iap.httpsResourceAccessor
```

IAP's own Google-managed sign-in admits only accounts in the project's organization, so give it
the service's Web client instead (add `https://iap.googleapis.com/v1/oauth/clientIds/<client
id>:handleRedirect` to that client's redirect URIs first), and let the app's desktop OAuth
client's ID tokens through (`programmatic_clients`, which must be in the same organization as the
project):

```sh
cat > iap.yaml <<EOF
access_settings:
  oauth_settings:
    client_id: <the Web client's id>
    client_secret: <its secret>
    programmatic_clients:
    - <the app's client id>
EOF
gcloud iap settings set iap.yaml --project $P --resource-type cloud-run --service relay-api \
    --region $R
rm iap.yaml
```

The consent screen must be **In production**: while it's Testing, Google drops the service's
refresh tokens after 7 days.

## The app

Release builds take the `relay-api` URL from the `EXO_CLOUD_URL` secret at build time. A Mac's
`~/Library/Application Support/Examail/cloud.json` overrides it with `{"url": "…"}`, or turns
Examail Cloud off with `{"enabled": false}`. Until an account connects (in the browser, from the
link in Examail's status bar, or `<relay-api URL>/v1/connect?login_hint=<address>`), its
reminders and scheduled sends wait for Examail to open.

## The mail watcher

Gmail publishes each connected inbox's changes to the Pub/Sub topic `gmail-push`, which pushes
them to the worker. The tick renews each account's `users.watch` before it lapses (a week) and
also reads what changed every minute, so a lost push only delays things. `cloud/deploy` turns the
watcher on (`GMAIL_TOPIC`) once the topic exists:

```sh
gcloud pubsub topics create gmail-push --project $P
gcloud pubsub topics add-iam-policy-binding gmail-push --project $P \
    --member serviceAccount:gmail-api-push@system.gserviceaccount.com --role roles/pubsub.publisher
gcloud iam service-accounts add-iam-policy-binding relay-scheduler@$P.iam.gserviceaccount.com \
    --project $P --member serviceAccount:service-$N@gcp-sa-pubsub.iam.gserviceaccount.com \
    --role roles/iam.serviceAccountTokenCreator
W=$(gcloud run services describe relay-worker --project $P --region $R --format 'value(status.url)')
gcloud pubsub subscriptions create gmail-push-worker --project $P --topic gmail-push \
    --push-endpoint $W/hooks/gmail --push-auth-service-account relay-scheduler@$P.iam.gserviceaccount.com \
    --push-auth-token-audience $W --ack-deadline 60 --message-retention-duration 1h \
    --expiration-period never
```

For each message newly in an inbox that isn't the user's own, the worker:

- takes a reply on a muted thread (`[Exo]/Muted`) back out of the inbox, unless it added one of
  the user's addresses as a new recipient;
- ends if-no-reply reminders and sends on that thread at once;
- with phone notifications on (⌘K → Turn On Phone Notifications, a random ntfy.sh topic in the
  synced settings), posts the sender and subject to that topic, by the Mac's notification rules;
- once Gmail's search has the message, labels it `[Exo]/<split>` for each split whose compiled
  Gmail query it matches, so Gmail on the web and phone can show the split.

## Settings sync

The app keeps each account's settings document on the service (`GET`/`PUT /v1/settings`, with a
version that a stale write fails on) and merges key by key against the copy both sides last had.
A Mac's first sync takes the service's settings and keeps its own in `kv`
(`cloud_settings_before_sync`). With each write the app sends the account's addresses and label
names, which the worker needs.

## Alerts

A log-based alert policy, "Relay job failed", emails when the worker logs `job <id> Failed` or
either service logs an error, at most once an hour. It was made through the Monitoring API: an
email notification channel, then an alert policy with a `conditionMatchedLog` on

```
resource.type="cloud_run_revision" AND resource.labels.service_name=("relay-worker" OR "relay-api")
AND (severity>=ERROR OR textPayload=~"job \\S+ Failed")
```

and a `notificationRateLimit` of an hour.

## Running locally

Without `IAP_AUDIENCE` the API trusts `X-Examail-Dev-User: <sub>:<email>` instead of IAP's
assertion. It still needs a real Firestore and KMS key; `GCP_ACCESS_TOKEN` stands in for the
metadata server:

```sh
ROLE=api PROJECT_ID=$P KMS_KEY=projects/$P/locations/$R/keyRings/relay/cryptoKeys/refresh-tokens \
    GCP_ACCESS_TOKEN=$(gcloud auth print-access-token) cargo run -p examail-cloud
```
