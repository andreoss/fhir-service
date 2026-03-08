#!/bin/sh


set -eu

image="${FHIR_IMAGE:-fhir-service:0.1.0}"

docker image inspect "$image" --format '{{.Id}}' 2>/dev/null || {
    echo "no image named $image" >&2
    exit 1
}
