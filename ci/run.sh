#!/bin/sh
set -eu

here="$(dirname "$0")"
cd "$here/.."
mkdir -p scratch
action="${1:-run}"
if [ "$action" = run ]; then
    trap 'docker compose down -v >/dev/null 2>&1 || true' EXIT INT TERM
fi
cargo run --quiet --bin pipeline -- "$action" ci/pipeline.yaml
