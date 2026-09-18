#!/bin/sh
# Nightly off-site copy of the licence database to Cloudflare R2.
#
# This database is the only record of who bought what: every partner, every
# activation key sold, and the license_term rows the monthly reconciliation
# bills from. The host holds one copy and nothing else does.
#
# The signing key is deliberately not in here. It already exists as the
# LICENSE_SIGNING_KEY repository secret and in the checkout, so a third copy
# inside the archive would only make the age identity the single point that
# loses every licence in the field at once.
#
# Everything runs inside the product's backup image, which already carries
# pg_dump, age and aws-cli. Installing three packages on the host to avoid one
# `podman run` would be the same tools kept in a second place.
set -eu

# A systemd user unit inherits the user manager's PATH, and an ssh command a
# non-login shell's; neither has ~/bin, where this host keeps the podman it
# calls below. Without this the run ends at `podman: not found`, and the timer
# reports it at half past four in the morning.
PATH=$HOME/bin:$PATH
export PATH

# Decided constants of this deployment, not values anybody has to invent:
# there is one licence server and one bucket.
BUCKET=laysi-one-license
IMAGE=ghcr.io/laysi/laysi-one-backup:latest
NETWORK=laysi-one-license_default
SECRETS_VOLUME=laysi-one-license_secrets

# The credentials and the recipient are produced during setup, so they live in
# ~/.config/laysi-license-backup.env, which systemd reads. Missing means the
# setup is incomplete, which is a failure rather than a backup of nothing.
: "${R2_ACCOUNT_ID:?not set; see license-server/deploy/README.md}"
: "${R2_ACCESS_KEY_ID:?not set; see license-server/deploy/README.md}"
: "${R2_SECRET_ACCESS_KEY:?not set; see license-server/deploy/README.md}"
: "${AGE_RECIPIENT:?not set; see license-server/deploy/README.md}"

stamp=$(date -u +%Y%m%dT%H%M%SZ)

# Renamed into the environment rather than passed as `-e NAME=value`: podman's
# arguments are in /proc/*/cmdline, which every local user can read, so a value
# spelled out there is a secret published for the length of the run. `-e NAME`
# with no value takes it from this process instead.
AWS_ACCESS_KEY_ID=$R2_ACCESS_KEY_ID
AWS_SECRET_ACCESS_KEY=$R2_SECRET_ACCESS_KEY
export AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY

# One container for the whole pipeline. Streaming the dump back out through
# podman's stdout only to pipe it in again would put the host in the middle of
# its own backup for no gain.
exec podman run --rm \
    --network "$NETWORK" \
    -v "$SECRETS_VOLUME:/secrets:ro" \
    -e AWS_ACCESS_KEY_ID \
    -e AWS_SECRET_ACCESS_KEY \
    -e AWS_DEFAULT_REGION=auto \
    -e "ENDPOINT=https://$R2_ACCOUNT_ID.r2.cloudflarestorage.com" \
    -e "RECIPIENT=$AGE_RECIPIENT" \
    -e "TARGET=s3://$BUCKET/db/license-$stamp.dump.age" \
    --entrypoint /bin/sh \
    "$IMAGE" -c '
        set -eu
        # A pg_dump that dies mid-stream otherwise leaves age writing a
        # well-formed archive of a truncated dump, and the run reports success.
        set -o pipefail

        # The dump lands in a file before it is uploaded. Piping pg_dump
        # straight at `aws s3 cp -` publishes whatever arrived before the
        # failure as a complete object, and the bucket lock then keeps that
        # truncated archive for 90 days.
        pg_dump -Fc "$(cat /secrets/database_url)" | age -r "$RECIPIENT" -o /tmp/dump.age
        bytes=$(wc -c < /tmp/dump.age)
        # age writes a header even for empty input, so "not empty" is not the
        # test; a dump of this schema does not come in under a kilobyte.
        [ "$bytes" -gt 1024 ] || { echo "dump is $bytes bytes; refusing to upload it" >&2; exit 1; }

        aws --endpoint-url "$ENDPOINT" s3 cp /tmp/dump.age "$TARGET"
        echo "$TARGET ($bytes bytes)"
    '
