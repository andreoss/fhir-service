#!/bin/sh
set -eu

here="$(dirname "$0")"
cd "$here/.."
mkdir -p scratch
exec cargo run --quiet --bin pipeline -- "${1:-run}" ci/pipeline.yaml
