#!/bin/sh












set -eu

here="$(dirname "$0")"
cd "$here/.."

kits="${FHIR_CONFORMANCE_KITS:-smart-kit-inferno:latest}"

missing=""
for image in $kits; do
    docker image inspect "$image" >/dev/null 2>&1 || missing="$missing $image"
done

if [ -n "$missing" ]; then
    echo "conformance: these kit images are not present:$missing" >&2
    echo "conformance: obtain them from their publishers and run again" >&2
    exit 1
fi

if [ -z "${FHIR_CONFORMANCE_BASE:-}" ]; then
    echo "conformance: FHIR_CONFORMANCE_BASE names the instance to judge; unset," >&2
    echo "             so there is nothing to run the kits against." >&2
    exit 1
fi

echo "conformance: judging $FHIR_CONFORMANCE_BASE"
docker run --rm --network host "${kits%% *}" \
    bundle exec inferno execute --suite smart_stu2_2 \
    --inputs "url:$FHIR_CONFORMANCE_BASE" --outputter plain
