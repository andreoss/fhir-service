#!/bin/sh
set -eu

here="$(dirname "$0")"
cd "$here/.."

kits="${FHIR_CONFORMANCE_KITS:-smart-kit-inferno:latest}"
publisher="${FHIR_CONFORMANCE_PUBLISHER:-https://github.com/inferno-framework/smart-app-launch-test-kit}"
revision="${FHIR_CONFORMANCE_REVISION:-v1.0.3}"
groups="${FHIR_CONFORMANCE_GROUPS:-3}"
preset="${FHIR_CONFORMANCE_PRESET:-ci/conformance/preset.json}"
work=scratch/conformance
tls="$work/tls"
data="$work/kit-data"

command -v docker >/dev/null 2>&1 || {
    echo "conformance: docker is not installed; this stage judges nothing" >&2
    exit 1
}

command -v openssl >/dev/null 2>&1 || {
    echo "conformance: openssl is not installed; the issuer cannot be given a certificate" >&2
    exit 1
}

command -v git >/dev/null 2>&1 || {
    echo "conformance: git is not installed; the kit cannot be obtained from its publisher" >&2
    exit 1
}

for image in $kits; do
    docker image inspect "$image" >/dev/null 2>&1 && continue
    echo "conformance: $image is absent; building it from $publisher at $revision"
    rm -rf "$work/kit"
    mkdir -p "$work"
    git clone --quiet --depth 1 --branch "$revision" "$publisher" "$work/kit" || {
        echo "conformance: the kit cannot be obtained from its publisher" >&2
        exit 1
    }
    docker build --quiet --tag "$image" "$work/kit" >/dev/null || {
        echo "conformance: the kit does not build from its own recipe" >&2
        exit 1
    }
done
kit="${kits%% *}"

mkdir -p "$tls" "$data"
openssl req -config ci/conformance/authority.cnf -x509 -newkey rsa:2048 -nodes \
    -keyout "$tls/ca-key.pem" -out "$tls/ca.pem" -days 30 >/dev/null 2>&1
openssl req -config ci/conformance/issuer.cnf -newkey rsa:2048 -nodes \
    -keyout "$tls/issuer-key.pem" -out "$tls/issuer.csr" >/dev/null 2>&1
openssl x509 -req -in "$tls/issuer.csr" -CA "$tls/ca.pem" -CAkey "$tls/ca-key.pem" \
    -CAcreateserial -out "$tls/issuer.pem" -days 30 \
    -extfile ci/conformance/issuer.cnf -extensions issuer >/dev/null 2>&1
chmod 644 "$tls"/*.pem

base="${FHIR_CONFORMANCE_BASE:-}"
network=host
if [ -z "$base" ]; then
    realm="https://issuer:8443/realms/fhir"
    FHIR_AUTH_ISSUER="$realm"
    FHIR_AUTH_AUTHORIZE="$realm/protocol/openid-connect/auth"
    FHIR_AUTH_TOKEN="$realm/protocol/openid-connect/token"
    FHIR_AUTH_INTROSPECT="$realm/protocol/openid-connect/token/introspect"
    FHIR_AUTH_JWKS="$realm/protocol/openid-connect/certs"
    FHIR_AUTH_AUDIENCE="http://service:8080"
    FHIR_AUTH_SCOPES="launch,launch/patient,patient/*.rs,user/*.rs,system/*.rs,fhirUser,openid"
    FHIR_AUTH_CAPABILITIES="launch-ehr,launch-standalone,client-public,client-confidential-symmetric,sso-openid-connect,context-standalone-patient,context-ehr-patient,permission-patient,permission-user,permission-v2,authorize-post"
    export FHIR_AUTH_ISSUER FHIR_AUTH_AUTHORIZE FHIR_AUTH_TOKEN FHIR_AUTH_INTROSPECT
    export FHIR_AUTH_JWKS FHIR_AUTH_AUDIENCE FHIR_AUTH_SCOPES FHIR_AUTH_CAPABILITIES
    trap 'docker compose --profile conformance stop issuer service >/dev/null 2>&1 || true' EXIT INT TERM
    docker compose --profile conformance up -d --wait --build || {
        echo "conformance: the instance and the issuer it is judged against did not come up" >&2
        exit 1
    }
    held="$(docker compose ps --format '{{.Name}}' service | head -1)"
    network="$(docker inspect "$held" --format '{{range $name, $held := .NetworkSettings.Networks}}{{$name}} {{end}}' | awk '{print $1}')"
    [ -n "$network" ] || {
        echo "conformance: the network the kit is driven over is not present" >&2
        exit 1
    }
    base="http://service:8080"
fi

docker run --rm -v "$PWD/$data:/opt/inferno/data" "$kit" \
    bundle exec inferno migrate >/dev/null 2>&1 || {
    echo "conformance: the kit's own database cannot be prepared" >&2
    exit 1
}

echo "conformance: judging $base with $kit over $network, groups $groups"
docker run --rm --network "$network" \
    -e SSL_CERT_FILE=/tls/ca.pem \
    -v "$PWD/$data:/opt/inferno/data" \
    -v "$PWD/$tls:/tls:ro" \
    -v "$PWD/$preset:/preset.json:ro" \
    "$kit" bundle exec inferno execute --suite smart_stu2_2 \
    --groups $groups --preset-file /preset.json \
    --inputs "url:$base" --outputter plain
