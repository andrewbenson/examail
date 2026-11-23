# Examail Cloud (Relay)

The service that does what Examail can only do while it's open (bring back snoozed threads, send
scheduled mail, apply mail rules) when the Mac is closed. The code is `crates/examail-cloud`; one
binary runs as both Cloud Run services:

- **`relay-api`**, behind IAP: what the app and the browser call. IAP admits only the Google
  accounts given `roles/iap.httpsResourceAccessor`, and the service checks IAP's signed assertion
  on every request.
- **`relay-worker`**, Cloud Run IAM only: Cloud Scheduler's tick and Gmail's Pub/Sub push.

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

and let the app's desktop OAuth client's ID tokens through (`programmatic_clients`, which must be
in the same organization as the project):

```sh
cat > iap.yaml <<EOF
access_settings:
  oauth_settings:
    programmatic_clients:
    - <the app's client id>
EOF
gcloud iap settings set iap.yaml --project $P --resource-type cloud-run --service relay-api \
    --region $R
```

## Running locally

Without `IAP_AUDIENCE` the API trusts `X-Examail-Dev-User: <sub>:<email>` instead of IAP's
assertion. It still needs a real Firestore and KMS key; `GCP_ACCESS_TOKEN` stands in for the
metadata server:

```sh
ROLE=api PROJECT_ID=$P KMS_KEY=projects/$P/locations/$R/keyRings/relay/cryptoKeys/refresh-tokens \
    GCP_ACCESS_TOKEN=$(gcloud auth print-access-token) cargo run -p examail-cloud
```
