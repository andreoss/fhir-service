#!/bin/sh








set -eu

here="$(dirname "$0")"
cd "$here/.."

command -v trivy >/dev/null 2>&1 || {
    echo "hardening: trivy is not installed; this stage judges nothing" >&2
    exit 1
}

report="${FHIR_HARDENING_REPORT:-scratch/hardening.json}"
mkdir -p "$(dirname "$report")"

trivy fs --scanners misconfig --quiet --format json \
    --skip-dirs target --skip-dirs scratch --skip-dirs doc \
    --skip-dirs fhir-server --skip-dirs references \
    -o "$report" . || {
    echo "hardening: the scan did not complete" >&2
    exit 1
}





node -e '
const held = require(require("path").resolve(process.argv[1]));
const known = new Set(["KSV-0013"]);
let bad = 0;
for (const r of (held.Results || [])) {
  for (const m of (r.Misconfigurations || [])) {
    const severe = m.Severity === "HIGH" || m.Severity === "CRITICAL";
    if (known.has(m.ID)) { console.log("known", m.ID, r.Target, "|", m.Title); continue; }
    console.log(m.Severity, m.ID, r.Target, "|", m.Title);
    if (severe) bad += 1;
  }
}
console.log("hardening: " + bad + " findings this stage fails on");
process.exit(bad === 0 ? 0 : 1);
' "$report"
