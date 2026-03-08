#!/bin/sh
set -eu

image="${FHIR_IMAGE:-fhir-service:0.1.0}"
registry="${FHIR_REGISTRY:-}"

docker build --file Containerfile --tag "$image" .

if [ -z "$registry" ]; then
    echo "publish skipped: no registry named"
    exit 0
fi

docker tag "$image" "$registry/$image"
docker push "$registry/$image"
echo "published $registry/$image"
