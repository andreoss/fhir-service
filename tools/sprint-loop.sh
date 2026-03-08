#!/usr/bin/env bash
set -uo pipefail

REPO="${REPO:-/user/Project/fhir-service}"
SPRINTS="$REPO/tools/sprints.tsv"
REVIEW="$REPO/doc/review"
TRACKER="$REPO/doc/Tracker.adoc"
TEMPLATE="$REPO/tools/claude-sprint.md"
TIMEOUT="${TIMEOUT:-2400}"
DB_FROM="${DB_FROM:-9}"

cd "$REPO" || exit 1

log() { printf '%s %s\n' "$(date -u +%H:%M:%S)" "$*"; }
note() { printf -- '- %s %s: %s\n' "$(date -u +%F)" "$1" "$2" >> "$TRACKER"; }
status() { grep -q "^Status: $2" "$REVIEW/$1.adoc" 2>/dev/null; }

verify() {
  [ -f "$REPO/Cargo.toml" ] || return 0
  cargo build --workspace -q || return 1
  cargo test --workspace -q || return 1
}

db_up() {
  [ "${1#0}" -ge "$DB_FROM" ] || return 0
  docker compose up -d --wait || return 1
}

commits_for() {
  local ids="$1" i
  for i in $(echo "$ids" | tr -d ' ' | tr ',' ' '); do
    git log --oneline -60 --grep="^$i:" | grep -q . && return 0
  done
  return 1
}

render() {
  sed -e "s|__ID__|$1|g" \
      -e "s|__IDS__|$2|g" \
      -e "s|__GOAL__|$3|g" \
      -e "s|__DONE__|$4|g" \
      -e "s|__VERIFY__|$5|g" \
      "$TEMPLATE" > "$REVIEW/$1.prompt"
}

run_claude() {
  timeout "$TIMEOUT" claude -p --dangerously-skip-permissions \
    --add-dir "$REPO" < "$REVIEW/$1.prompt" > "$REVIEW/$1.log" 2>&1
}

run_opencode() {
  local model
  for model in opencode/big-pickle opencode/mimo-v2.5-free \
               opencode/nemotron-3.5-lightning-free; do
    log "  fallback dispatch $1 via $model"
    if timeout "$TIMEOUT" opencode run --dir "$REPO" -m "$model" \
        "$(cat "$REVIEW/$1.prompt")" > "$REVIEW/$1.log" 2>&1 < /dev/null; then
      return 0
    fi
    log "  $model failed"
  done
  return 1
}

while IFS=$'\t' read -r id ids goal done verify_cmd <&3; do
  [ -z "${id:-}" ] && continue
  [ "${id:0:1}" = "#" ] && continue

  if status "$id" done; then log "skip $id done"; continue; fi

  log "start $id ($ids)"
  if ! db_up "${id#S}"; then
    note "$id" "blocked: backing services did not start"
    log "stop: $id blocked, services unavailable"
    break
  fi

  render "$id" "$ids" "$goal" "$done" "$verify_cmd"

  log "  delegate $id to claude"
  run_claude "$id" || { log "  claude failed"; run_opencode "$id" || {
    note "$id" "blocked: claude and every fallback failed"
    log "stop: $id blocked, no runner completed"
    break
  }; }

  if ! status "$id" done; then
    note "$id" "blocked: report is not done"
    log "stop: $id blocked, report missing or not done"
    break
  fi

  if ! verify; then
    note "$id" "blocked: build or tests failed"
    log "stop: $id blocked, verification failed"
    break
  fi

  if ! commits_for "$ids"; then
    note "$id" "blocked: no commit for its ids"
    log "stop: $id blocked, nothing committed"
    break
  fi

  note "$id" "done ($ids)"
  log "done $id"
done 3< "$SPRINTS"

log "loop finished"
