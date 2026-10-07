#!/usr/bin/env bash
# Checks rec export --upload against a local PUT server: presigned URLs and the Vercel Blob API.
# Needs: a built rec (REC=path, default target/release/rec), ffmpeg, python3.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
REC=${REC:-$here/../target/release/rec}
OUT=${OUT:-$(mktemp -d "${TMPDIR:-/tmp}/rec-upload.XXXXXX")}
LOG="$OUT/requests.jsonl"
SERVER=""
export BASH_SILENCE_DEPRECATION_WARNING=1

cleanup() { [ -n "$SERVER" ] && kill "$SERVER" 2>/dev/null || true; }
trap cleanup EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

json() {
  python3 -c 'import json,sys; j=json.load(sys.stdin); v='"$1"'; print(v if not isinstance(v,(dict,list)) else json.dumps(v))'
}

check() {
  local what=$1 expr=$2 input=$3
  [ "$(printf '%s' "$input" | json "$expr")" = "True" ] || fail "$what: $input"
  echo "ok: $what" >&2
}

last_request() { tail -1 "$LOG"; }

[ -x "$REC" ] || fail "rec binary not found at $REC (cargo build --release)"
mkdir -p "$OUT"
: >"$LOG"
python3 "$here/put_server.py" "$LOG" >"$OUT/port" &
SERVER=$!
for _ in $(seq 50); do [ -s "$OUT/port" ] && break; sleep 0.1; done
PORT=$(cat "$OUT/port")
[ -n "$PORT" ] || fail "put server did not start"
BASE="http://127.0.0.1:$PORT"

# Output after the first second, so the export passes review's static check.
recorded=$("$REC" record --tty --size 40x10 --duration 3 --out "$OUT" -- sh -c 'echo upload-check; sleep 1.5; echo still-here; sleep 1' 2>/dev/null) ||
  fail "record failed: $recorded"
TAKE_ID=$(printf '%s' "$recorded" | json 'j["id"]')
TAKE="$OUT/$TAKE_ID"
[ -d "$TAKE" ] || fail "no take folder at $TAKE"

out=$("$REC" export "$TAKE" --upload "$BASE/up/take.mp4?X-Amz-Signature=abc&X-Amz-Expires=600" 2>/dev/null) &&
  fail "presigned upload with two layouts succeeded"
check "presigned with two layouts is bad_args" 'j["error"]["code"] == "bad_args"' "$out"
[ -s "$LOG" ] && fail "bad_args still sent a request"
[ -e "$TAKE/export-16x9.mp4" ] && fail "bad_args still exported"

out=$("$REC" export "$TAKE" --layout 16:9 --upload "$BASE/up/take.mp4?X-Amz-Signature=abc&X-Amz-Expires=600" 2>/dev/null) ||
  fail "presigned upload failed: $out"
mp4="$TAKE/export-16x9.mp4"
size=$(wc -c <"$mp4" | tr -d ' ')
sha=$(python3 -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$mp4")
check "presigned url is reported without its query" 'j["exports"][0]["url"] == "'"$BASE"'/up/take.mp4"' "$out"
req=$(last_request)
check "presigned sends one PUT to the signed URL" \
  'j["method"] == "PUT" and j["path"] == "/up/take.mp4?X-Amz-Signature=abc&X-Amz-Expires=600"' "$req"
check "presigned sends the whole file ($size bytes)" \
  'j["length"] == '"$size"' and j["sha256"] == "'"$sha"'" and j["headers"]["content-length"] == "'"$size"'"' "$req"
check "presigned is not chunked and is typed video/mp4" \
  '"transfer-encoding" not in j["headers"] and j["headers"]["content-type"] == "video/mp4"' "$req"

out=$("$REC" export "$TAKE" --layout 16:9 --upload "$BASE/deny/take.mp4?sig=old" 2>/dev/null) &&
  fail "upload to a 403 URL succeeded"
check "a refused upload is upload_failed with the status and body" \
  'j["error"]["code"] == "upload_failed" and "403" in j["error"]["message"] and "Request has expired" in j["error"]["message"]' "$out"

out=$(env -u BLOB_READ_WRITE_TOKEN "$REC" export "$TAKE" --upload blob 2>/dev/null) && fail "blob without a token succeeded"
check "blob without a token names the variable" \
  'j["error"]["code"] == "missing_credentials" and "BLOB_READ_WRITE_TOKEN" in j["error"]["message"]' "$out"

: >"$LOG"
TOKEN=vercel_blob_rw_store1_s3cr3t
out=$(BLOB_READ_WRITE_TOKEN=$TOKEN REC_BLOB_API_URL="$BASE/api/blob" "$REC" export "$TAKE" --upload blob 2>/dev/null) ||
  fail "blob upload failed: $out"
check "blob reports the URL the API returned for each layout" \
  '[e["url"] for e in j["exports"]] == ["https://store1.public.blob.vercel-storage.com/rec/'"$TAKE_ID"'/export-" + s + ".mp4" for s in ("16x9", "9x16")]' "$out"
[ "$(wc -l <"$LOG" | tr -d ' ')" = 2 ] || fail "blob sent $(wc -l <"$LOG") requests, wanted 2"
req=$(head -1 "$LOG")
size=$(wc -c <"$TAKE/export-16x9.mp4" | tr -d ' ')
check "blob PUTs to /?pathname= like @vercel/blob put()" \
  'j["method"] == "PUT" and j["path"] == "/api/blob/?pathname=rec%2F'"$TAKE_ID"'%2Fexport-16x9.mp4"' "$req"
check "blob sends the headers @vercel/blob sends" \
  '{k: j["headers"].get(k) for k in ("authorization", "x-api-version", "x-vercel-blob-store-id", "x-api-blob-request-attempt", "x-vercel-blob-access", "x-content-type", "x-add-random-suffix", "x-allow-overwrite")} == {"authorization": "Bearer '"$TOKEN"'", "x-api-version": "12", "x-vercel-blob-store-id": "store1", "x-api-blob-request-attempt": "0", "x-vercel-blob-access": "public", "x-content-type": "video/mp4", "x-add-random-suffix": "0", "x-allow-overwrite": "1"}' "$req"
check "blob request id is <store>:<ms>:<hex>" \
  '__import__("re").fullmatch(r"store1:\d{13}:[0-9a-f]+", j["headers"]["x-api-blob-request-id"]) is not None' "$req"
check "blob sends the whole file" 'j["length"] == '"$size"' and j["headers"]["content-length"] == "'"$size"'"' "$req"

# A take where nothing happens fails review, so --upload sends nothing and keeps the files.
frozen=$("$REC" record --tty --size 40x10 --duration 3 --out "$OUT" -- sleep 3 2>/dev/null) ||
  fail "record failed: $frozen"
FROZEN_ID=$(printf '%s' "$frozen" | json 'j["id"]')
FROZEN="$OUT/$FROZEN_ID"
: >"$LOG"
out=$("$REC" export "$FROZEN" --layout 9:16 --upload "$BASE/up/frozen.mp4?sig=1" 2>/dev/null) &&
  fail "a video that failed review was uploaded: $out"
check "a static video is review_failed naming the check and the sheet" \
  'j["error"]["code"] == "review_failed" and "static" in j["error"]["message"] and "'"$FROZEN_ID"'/export-9x16.sheet.png" in j["error"]["message"]' "$out"
[ -s "$LOG" ] && fail "review_failed still sent a request"
[ -s "$FROZEN/export-9x16.mp4" ] && [ -s "$FROZEN/export-9x16.sheet.png" ] || fail "review_failed did not keep the mp4 and sheet"
echo "ok: nothing uploaded, files kept" >&2

echo "PASS: $OUT"
