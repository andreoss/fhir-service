#!/usr/bin/env bash
set -uo pipefail

REPO=/user/Project/fhir-service
cd "$REPO" || exit 1

export TIMEOUT=2400
export REPO

mkdir -p doc/review

exec claude -p \
  --dangerously-skip-permissions \
  --add-dir "$REPO" \
  < tools/claude-loop.md \
  > doc/review/orchestrator.log 2>&1
