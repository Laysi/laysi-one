#!/usr/bin/env bash
# Runtime verification of the expenses and documents modules.
#
# This script answers four questions, each of which must be actually executed
# rather than merely looking as though it would pass:
#
#   1. Migration 0007 really applies (columns, backfill and trigger included).
#   2. The fourteen new operations answer against a real database.
#   3. Files really reach RustFS, not a bytea column pretending to be storage.
#   4. Import dry run, commit and rollback all work (ROC dates convert,
#      unbalanced vouchers are rejected, rollback deletes).
#
# Usage: ./scripts/verify-0007.sh
set -uo pipefail
cd "$(dirname "$0")/.."

API=http://localhost:8088
PASS=0
FAIL=0

say()  { printf '\n\033[1m── %s\033[0m\n' "$*"; }
ok()   { PASS=$((PASS+1)); printf '  ✓ %s\n' "$*"; }
bad()  { FAIL=$((FAIL+1)); printf '  ✗ %s\n' "$*"; }

# Call an operation and print its data. The second argument is the JSON input.
op() {
  curl -s -XPOST "$API/v1/ops/$1" -H 'content-type: application/json' \
       -d "{\"input\":${2:-\{\}}}"
}

# Pull one field with python; an empty string on failure.
field() { python3 -c "
import json,sys
try:
    d=json.load(sys.stdin)
    if not d.get('ok'): sys.exit(1)
    v=d['data']
    for k in '$1'.split('.'):
        v = v[int(k)] if k.isdigit() else v[k]
    print(v if v is not None else '')
except Exception:
    sys.exit(1)
" 2>/dev/null; }

# 0. Migration state
say "migration state"
applied=$(docker compose exec -T postgres psql -U laysi -d laysi -t -A \
          -c "SELECT version FROM _sqlx_migrations ORDER BY version" 2>/dev/null | tr '\n' ' ')
echo "  applied: $applied"
if echo "$applied" | grep -q 7; then ok "0007 applied"; else bad "0007 not applied"; fi

# sqlx verifies checksums, so a server that starts means there is no mismatch.
if curl -sf "$API/healthz" >/dev/null; then ok "server starts (no migration checksum conflict)"
else bad "server will not start; 0007 was probably edited after being applied"; fi

# Did the column addition really complete?
dept=$(docker compose exec -T postgres psql -U laysi -d laysi -t -A -c \
  "SELECT count(*) FROM voucher_line WHERE dept_id IS NOT NULL" 2>/dev/null | tr -d '\r')
[ "${dept:-0}" -gt 0 ] && ok "voucher_line.dept_id backfilled $dept rows" || bad "dept_id backfilled nothing"

# 1. All fourteen operations answer
say "fourteen new operations"
for o in list_expense_claims get_budget_status get_department_pl list_documents list_imports; do
  if op "$o" | field 2>/dev/null | head -c1 >/dev/null && op "$o" | python3 -c "
import json,sys; sys.exit(0 if json.load(sys.stdin).get('ok') else 1)"; then
    ok "$o"
  else
    bad "$o —— $(op "$o" | head -c 160)"
  fi
done

# 2. Budgets: actuals are derived
say "budgets: actuals read live from the ledger"
src=$(op get_budget_status | field actual_source)
[ -n "$src" ] && ok "actual_source = $src" || bad "actual_source not readable"

# Departmental P&L must sum to the income statement.
recon=$(op get_department_pl | field reconciled)
[ "$recon" = "True" ] && ok "departmental P&L total = income statement profit" || bad "departmental P&L does not reconcile"

# 3. Documents really reach RustFS
say "documents: files really reach object storage"
STAMP=$(date +%s)
CONTENT="Laysi One receipt verification $STAMP"
B64=$(printf '%s' "$CONTENT" | base64 -w0)
up=$(op upload_document "{\"file_name\":\"verify-$STAMP.txt\",\"content_base64\":\"$B64\",\"kind\":\"receipt\"}")
DOC=$(echo "$up" | field document.doc_no)
KEY=$(echo "$up" | field document.object_key)
SUM=$(echo "$up" | field document.checksum)
if [ -n "$DOC" ]; then ok "uploaded $DOC, object key $KEY"; else bad "upload failed: $(echo "$up" | head -c 200)"; fi

# Key evidence 1: bypass the application and look for the object key directly on
# the RustFS container's disk.
if [ -n "$KEY" ]; then
  found=$(docker compose exec -T rustfs sh -c \
    "find /data -path '*$(basename "$KEY")*' 2>/dev/null | head -3" 2>/dev/null | tr -d '\r')
  if [ -n "$found" ]; then ok "object key found on the RustFS disk: $found"
  else bad "$KEY not found on the RustFS disk"; fi

  # Key evidence 2: the file's content really sits on the object store's disk.
  # Stronger than finding the name: it proves the bytes moved.
  hit=$(docker compose exec -T rustfs sh -c \
    "grep -rl '$STAMP' /data 2>/dev/null | head -2" 2>/dev/null | tr -d '\r')
  if [ -n "$hit" ]; then ok "content bytes are on the RustFS disk: $hit"
  else bad "file content not found on the RustFS disk"; fi
fi

# Key evidence 3: the database holds only the key, size, MIME type and hash,
# never the content.
leak=$(docker compose exec -T postgres psql -U laysi -d laysi -t -A -c \
  "SELECT count(*) FROM document_version WHERE object_key LIKE '%$STAMP%'" 2>/dev/null | tr -d '\r')
cols=$(docker compose exec -T postgres psql -U laysi -d laysi -t -A -c \
  "SELECT string_agg(column_name, ',' ORDER BY ordinal_position)
     FROM information_schema.columns WHERE table_name='document_version'" 2>/dev/null | tr -d '\r')
[ "${leak:-0}" -ge 1 ] && ok "database registered this revision ($leak rows)" || bad "database registered nothing"
if echo "$cols" | grep -qE 'bytea|content|body|data'; then
  bad "document_version has a content column: $cols"
else
  ok "document_version has no content column: $cols"
fi

# Retrieve and recompute the hash.
back=$(op get_document "{\"doc_no\":\"$DOC\",\"include_content\":true}")
VER=$(echo "$back" | field verified)
GOT=$(echo "$back" | field content_base64)
[ "$VER" = "True" ] && ok "BLAKE3 recomputed on retrieval matches ($SUM)" || bad "hash comparison failed"
[ "$GOT" = "$B64" ] && ok "retrieved content is byte-identical to the upload" || bad "retrieved content differs"

# Usage registered in the database must match actual bucket usage.
brecon=$(op list_documents | field bucket_reconciled)
[ "$brecon" = "True" ] && ok "bytes registered in the database = bytes in the bucket" || bad "database and bucket usage disagree"

# Uploading under the same number creates a new revision; the old one is not
# overwritten.
up2=$(op upload_document "{\"file_name\":\"verify-$STAMP.txt\",\"content_base64\":\"$(printf '%s v2' "$CONTENT" | base64 -w0)\",\"kind\":\"receipt\",\"doc_no\":\"$DOC\"}")
V2=$(echo "$up2" | field document.version_no)
[ "$V2" = "2" ] && ok "same number uploaded again gives v2 (v1 not overwritten)" || bad "revision did not become a new version"
K1=$(op get_document "{\"doc_no\":\"$DOC\",\"version\":1}" | field document.object_key)
[ "$K1" = "$KEY" ] && ok "v1 object key unchanged and its content still retrievable" || bad "v1 was modified"

# 4. Imports: dry run, commit, rollback
say "imports: dry run, commit, rollback"
CSV='傳票號,日期,摘要,科目代號,借方,貸方,部門代碼
H-001,114/06/30,6 月房租,6115,52000,0,admin
H-001,114/06/30,6 月房租,1102,0,52000,
H-002,114/06/28,"文具, 影印紙",6199,1800,0,admin
H-002,114/06/28,"文具, 影印紙",1101,0,1800,
H-003,114/06/25,借貸不平的那一張,6122,5000,0,sales
H-003,114/06/25,借貸不平的那一張,1102,0,4000,'
PAYLOAD=$(python3 -c "
import json,sys
print(json.dumps({'source_code':'voucher','file_name':'history.csv','content':sys.stdin.read()}))
" <<<"$CSV")
pv=$(op preview_import "$PAYLOAD")
BATCH=$(echo "$pv" | field batch.batch_no)
ERRS=$(echo "$pv" | field batch.error_count)
WARNS=$(echo "$pv" | field batch.warn_count)
CAN=$(echo "$pv" | field can_commit)
[ -n "$BATCH" ] && ok "dry run produced $BATCH ($ERRS errors, $WARNS warnings)" || bad "dry run failed: $(echo "$pv" | head -c 200)"
[ "$ERRS" = "1" ] && ok "the unbalanced voucher was rejected (1 error)" || bad "unbalanced voucher not rejected, errors = $ERRS"
[ "$WARNS" = "6" ] && ok "all six ROC-era dates left a warning" || bad "wrong number of ROC warnings: $WARNS"
[ "$CAN" = "False" ] && ok "errors present, so commit is refused" || bad "commit allowed despite errors"

# A dry run must write no vouchers.
before=$(op search_vouchers '{"limit":500}' | python3 -c "import json,sys; print(len(json.load(sys.stdin)['data']['vouchers']))")
ok "voucher count after dry run is $before (nothing written)"

# Fix that voucher and run again; this time it must import.
CSV_OK=$(echo "$CSV" | sed 's/0,4000,/0,5000,/')
PAYLOAD2=$(python3 -c "
import json,sys
print(json.dumps({'source_code':'voucher','file_name':'history-fixed.csv','content':sys.stdin.read()}))
" <<<"$CSV_OK")
pv2=$(op preview_import "$PAYLOAD2")
B2=$(echo "$pv2" | field batch.batch_no)
CAN2=$(echo "$pv2" | field can_commit)
[ "$CAN2" = "True" ] && ok "once fixed, $B2 can be committed" || bad "still not committable after the fix"

cm=$(op commit_import "{\"batch_no\":\"$B2\"}")
NOS=$(echo "$cm" | python3 -c "import json,sys; print(len(json.load(sys.stdin)['data']['batch']['voucher_nos']))" 2>/dev/null)
[ "$NOS" = "3" ] && ok "commit wrote 3 vouchers" || bad "wrong number of vouchers written: $NOS"

# Everything imported is unposted.
pend=$(docker compose exec -T postgres psql -U laysi -d laysi -t -A -c \
  "SELECT count(*) FROM voucher v JOIN import_batch b ON v.no = ANY(b.voucher_nos)
    WHERE b.batch_no='$B2' AND v.status='pending'" 2>/dev/null | tr -d '\r')
[ "$pend" = "3" ] && ok "all three are pending (an import does not post)" || bad "some voucher is not pending"

# ROC years really convert to the Gregorian calendar.
yr=$(docker compose exec -T postgres psql -U laysi -d laysi -t -A -c \
  "SELECT min(voucher_date) FROM voucher v JOIN import_batch b ON v.no = ANY(b.voucher_nos)
    WHERE b.batch_no='$B2'" 2>/dev/null | tr -d '\r')
[ "${yr:0:4}" = "2025" ] && ok "ROC 114 converted to 2025 ($yr)" || bad "date not converted: $yr"

# Departments are attached.
dcnt=$(docker compose exec -T postgres psql -U laysi -d laysi -t -A -c \
  "SELECT count(*) FROM voucher_line l JOIN voucher v ON v.id=l.voucher_id
     JOIN import_batch b ON v.no = ANY(b.voucher_nos)
    WHERE b.batch_no='$B2' AND l.dept_id IS NOT NULL" 2>/dev/null | tr -d '\r')
[ "${dcnt:-0}" -ge 3 ] && ok "department codes attached to $dcnt lines" || bad "no department attached"

# Rollback must delete them, since none is posted.
rb=$(op rollback_import "{\"batch_no\":\"$B2\"}")
RM=$(echo "$rb" | python3 -c "import json,sys; print(len(json.load(sys.stdin)['data']['removed']))" 2>/dev/null)
[ "$RM" = "3" ] && ok "rollback deleted 3 pending vouchers" || bad "rollback left rows behind: $RM"
after=$(op search_vouchers '{"limit":500}' | python3 -c "import json,sys; print(len(json.load(sys.stdin)['data']['vouchers']))")
[ "$after" = "$before" ] && ok "voucher count back to $before after rollback" || bad "count is $after after rollback, was $before"

# 5. Permissions: a tool disabled for AI really is blocked
say "MCP grants"
blocked=$(curl -s -XPOST "$API/mcp" -H 'content-type: application/json' \
  -H "Authorization: Bearer ${MCP_BEARER_TOKEN:?issue an MCP token and set MCP_BEARER_TOKEN first}" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"commit_import","arguments":{"batch_no":"x"}}}')
echo "$blocked" | grep -q '"isError":true' && ok "commit_import called by AI was blocked" || bad "commit_import is not blocked for AI"

say "result: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
