#!/usr/bin/env bash
set -uo pipefail

REPO=/user/Project/fhir-service
cd "$REPO" || exit 1

export TIMEOUT=2400
export REPO

LOG=doc/review/loop.log
mkdir -p doc/review scratch

total=$(grep -c '^S[0-9]' tools/sprints.tsv)
done_count() { grep -l '^Status: done' doc/review/S*.adoc 2>/dev/null | wc -l; }

attempt=0
while true; do
  echo "=== $(date -u +%H:%M:%S) loop start (done $(done_count)/$total)" >> "$LOG"
  setsid bash tools/sprint-loop.sh >> "$LOG" 2>&1 < /dev/null &
  loop_pid=$!
  wait "$loop_pid"

  if [ "$(done_count)" -ge "$total" ]; then
    echo "=== $(date -u +%H:%M:%S) all sprints done" >> "$LOG"
    break
  fi

  attempt=$((attempt + 1))
  if [ "$attempt" -gt 6 ]; then
    echo "=== $(date -u +%H:%M:%S) giving up after $attempt unblock attempts" >> "$LOG"
    break
  fi

  echo "=== $(date -u +%H:%M:%S) claude unblock attempt $attempt" >> "$LOG"
  claude -p --dangerously-skip-permissions --add-dir "$REPO" \
    < tools/claude-unblock.md \
    >> doc/review/orchestrator.log 2>&1
done
