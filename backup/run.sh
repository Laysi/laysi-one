#!/bin/sh
# Off-site backup loop. Reads its destination from backup_setting, so nothing
# here is configured on the host and a change of destination needs no restart.
#
# Two buckets are in play with different credentials: the local object store
# to read documents from, and the remote destination to write to. Neither set
# is exported, because exporting one would silently be used for the other.

set -eu

INTERVAL_SECONDS="${BACKUP_INTERVAL_SECONDS:-86400}"
# Same convention as the server binaries: compose cannot interpolate a secret
# it did not have before the containers started, so it comes from a file.
[ -n "${DATABASE_URL:-}" ] || DATABASE_URL=$(cat "$DATABASE_URL_FILE")

# One field at a time. A single row split on a separator would break the day a
# secret key contains that character, and this runs once a day.
get() {
    psql "$DATABASE_URL" -qtAX -c "SELECT $1 FROM backup_setting WHERE id = 1"
}

note_result() {
    psql "$DATABASE_URL" -qtAX -c \
        "UPDATE backup_setting
            SET last_run_at = now(), last_error = \$err\$$1\$err\$, last_bytes = $2
          WHERE id = 1" > /dev/null
}

# Reading the documents bucket.
here() {
    AWS_ACCESS_KEY_ID="$RUSTFS_ACCESS_KEY" \
    AWS_SECRET_ACCESS_KEY="$RUSTFS_SECRET_KEY" \
    AWS_DEFAULT_REGION=us-east-1 \
        aws --endpoint-url "$RUSTFS_ENDPOINT" "$@"
}

# Writing the off-site copy.
there() {
    AWS_ACCESS_KEY_ID="$access_key" \
    AWS_SECRET_ACCESS_KEY="$secret_key" \
    AWS_DEFAULT_REGION="$region" \
        aws --endpoint-url "$endpoint" "$@"
}

# The container owns age, so it is what generates the key pair: the format is
# the one thing that must not be guessed, and a wrong one is only discovered
# when a restore is attempted. The private half goes to age_identity_pending
# for the screen to show once, and run_once refuses to back up while it sits
# there -- so no archive ever contains the key that opens it.
ensure_key() {
    [ -z "$(get age_recipient)" ] || return 0

    age-keygen -o /tmp/identity.txt 2>/dev/null
    identity=$(grep -v '^#' /tmp/identity.txt | tr -d '\n')
    public=$(age-keygen -y /tmp/identity.txt)
    rm -f /tmp/identity.txt

    psql "$DATABASE_URL" -qtAX -c \
        "UPDATE backup_setting
            SET age_recipient = \$k\$$public\$k\$, age_identity_pending = \$k\$$identity\$k\$
          WHERE id = 1" > /dev/null
    echo "已產生還原金鑰，請到安全與營運頁保存後才會開始備份。"
}

run_once() {
    [ "$(get enabled)" = "t" ] || return 0

    endpoint=$(get endpoint)
    bucket=$(get bucket)
    [ -n "$bucket" ] && [ -n "$endpoint" ] || return 0

    ensure_key
    recipient=$(get age_recipient)
    [ -n "$recipient" ] || return 0
    # The archive must never contain the key that opens it.
    [ -z "$(get age_identity_pending)" ] || return 0

    region=$(get region)
    prefix=$(get prefix)
    access_key=$(get access_key)
    secret_key=$(get secret_key)
    keep_days=$(get keep_days)
    base="s3://$bucket/${prefix:+$prefix/}"

    # PostgreSQL first, then the objects. An object key is written to the
    # bucket before the database row that names it, so the bucket only grows;
    # dumping first means the copy can name an object the mirror has not
    # reached yet, which the next pass picks up. The other order would produce
    # a row pointing at an object no copy holds.
    stamp=$(date -u +%Y%m%dT%H%M%SZ)
    dump="/tmp/laysi-$stamp.dump.age"
    if ! pg_dump -Fc "$DATABASE_URL" | age -r "$recipient" -o "$dump"; then
        rm -f "$dump"
        note_result "資料庫備份失敗" NULL
        return 1
    fi
    bytes=$(wc -c < "$dump")
    if ! there s3 cp "$dump" "${base}db/laysi-$stamp.dump.age"; then
        rm -f "$dump"
        note_result "無法寫入遠端儲存位置" NULL
        return 1
    fi
    rm -f "$dump"

    # Documents travel encrypted too: a scanned contract is no less sensitive
    # than the dump. Each object is encrypted once and never again, which is
    # sound because an object never changes after it is written. So "already
    # in the remote" means "already done", and the pass stays incremental even
    # though age output differs every time.
    there s3 ls "${base}objects/" --recursive 2>/dev/null | awk '{print $4}' \
        | sed "s|^${prefix:+$prefix/}objects/||;s|\.age$||" | sort > /tmp/have
    here s3 ls "s3://$RUSTFS_BUCKET" --recursive 2>/dev/null | awk '{print $4}' \
        | sort > /tmp/want
    comm -13 /tmp/have /tmp/want > /tmp/todo
    while read -r key; do
        [ -n "$key" ] || continue
        if ! here s3 cp "s3://$RUSTFS_BUCKET/$key" - \
            | age -r "$recipient" \
            | there s3 cp - "${base}objects/$key.age"; then
            note_result "文件複製中斷，下一次會接著做" "$bytes"
            return 1
        fi
    done < /tmp/todo
    rm -f /tmp/have /tmp/want /tmp/todo

    # Retention applies to the dumps only. An object is the only copy of a
    # document the database still points at, so objects are never expired.
    cutoff=$(date -u -d "-${keep_days} days" +%Y%m%dT%H%M%SZ 2>/dev/null || echo "")
    if [ -n "$cutoff" ]; then
        there s3 ls "${base}db/" | awk '{print $4}' | while read -r name; do
            when=$(echo "$name" | sed -n 's|^laysi-\(.*\)\.dump\.age$|\1|p')
            [ -n "$when" ] || continue
            [ "$when" \< "$cutoff" ] && there s3 rm "${base}db/$name"
        done
    fi

    note_result "" "$bytes"
}

while true; do
    run_once || echo "備份未完成，將於下一次重試。"
    sleep "$INTERVAL_SECONDS"
done
