#!/bin/sh
set -eu

image="${FHIR_IMAGE:-fhir-service:0.1.0}"
registry="${FHIR_REGISTRY:-}"

BUILDAH_FORMAT=docker docker build --file Containerfile --tag "$image" .

if ! docker image inspect "$image" | grep -q '/usr/local/bin/probe'; then
    echo "the image carries no health check: it was built in a format that drops one" >&2
    exit 1
fi

if [ -z "$registry" ]; then
    echo "publish skipped: no registry named"
    exit 0
fi

docker tag "$image" "$registry/$image"
docker push "$registry/$image"
echo "published $registry/$image"
