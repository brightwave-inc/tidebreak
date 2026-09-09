#!/usr/bin/env bash
set -euo pipefail

# Delete hosted staging desktop prefixes that are not the live feed version
# and are not among the newest retained neighbors. Never touches production
# keys. AWS credentials and DOWNLOADS_S3_BUCKET must already be configured.

here="$(cd "$(dirname "$0")" && pwd)"
planner="$here/prune-staging-releases.mjs"

dry_run=false
case "${STAGING_PRUNE_DRY_RUN:-false}" in
  true|1|yes) dry_run=true ;;
  false|0|no|"") dry_run=false ;;
  *)
    echo "invalid STAGING_PRUNE_DRY_RUN: ${STAGING_PRUNE_DRY_RUN}" >&2
    exit 1
    ;;
esac

keep="${STAGING_PRUNE_KEEP:-3}"
bucket="${DOWNLOADS_S3_BUCKET:?DOWNLOADS_S3_BUCKET is required}"
work="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/tidebreak-staging-prune.XXXXXX")"
cleanup() { rm -rf -- "$work"; }
trap cleanup EXIT

head_error="$work/latest-head-error"
if ! aws s3api head-object \
  --bucket "$bucket" \
  --key tidebreak/staging/latest.json >/dev/null 2>"$head_error"; then
  error="$(<"$head_error")"
  case "$error" in
    *404*|*Not\ Found*)
      echo "No hosted staging latest.json; refusing to prune."
      exit 0
      ;;
    *)
      printf '%s\n' "$error" >&2
      exit 1
      ;;
  esac
fi

aws s3 cp \
  "s3://$bucket/tidebreak/staging/latest.json" \
  "$work/latest.json" >/dev/null
latest="$(jq -er .version "$work/latest.json")"
node "$here/staging-version.mjs" "$latest" >/dev/null

prefixes_file="$work/prefixes.txt"
: > "$prefixes_file"
continuation=""
while true; do
  args=(
    s3api list-objects-v2
    --bucket "$bucket"
    --prefix tidebreak/staging/releases/
    --delimiter /
    --output json
  )
  if [[ -n "$continuation" ]]; then
    args+=(--continuation-token "$continuation")
  fi
  response="$(aws "${args[@]}")"
  jq -r '.CommonPrefixes[]?.Prefix // empty' <<<"$response" >> "$prefixes_file"
  continuation="$(jq -r '.NextContinuationToken // empty' <<<"$response")"
  [[ -n "$continuation" ]] || break
done

plan_file="$work/plan.json"
node "$planner" --plan --latest-version "$latest" --keep "$keep" \
  < "$prefixes_file" > "$plan_file"
jq . "$plan_file"

keep_count="$(jq -r '.keep | length' "$plan_file")"
delete_count="$(jq -r '.delete | length' "$plan_file")"
skipped_count="$(jq -r '.skipped | length' "$plan_file")"
printf 'Staging prune plan: keep %s, delete %s, skipped %s (live %s).\n' \
  "$keep_count" "$delete_count" "$skipped_count" "$latest"

if [[ "$dry_run" = true ]]; then
  echo "Dry run; not deleting."
  exit 0
fi

if (( delete_count == 0 )); then
  echo "Nothing to delete."
  exit 0
fi

while IFS= read -r prefix; do
  [[ -n "$prefix" ]] || continue
  if ! aws s3 cp \
    "s3://$bucket/tidebreak/staging/latest.json" \
    "$work/latest.json" >/dev/null; then
    echo "Failed to re-read staging latest.json before deleting $prefix." >&2
    exit 1
  fi
  live="$(jq -er .version "$work/latest.json")"
  node "$planner" --assert-delete-prefix "$prefix" --latest-version "$live"
  echo "Deleting s3://$bucket/$prefix"
  aws s3 rm "s3://$bucket/$prefix" --recursive
done < <(jq -r '.delete[]' "$plan_file")
