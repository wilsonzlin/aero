#!/usr/bin/env bash
set -euo pipefail

# Local reproduction helper for `.github/workflows/iac.yml`.
#
# This is intentionally strict and will exit non-zero if any checks fail.

require_cmd() {
  local cmd="$1"
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "error: missing required command: $cmd" >&2
    return 1
  fi
}

require_cmd node
require_cmd docker
require_cmd terraform
require_cmd tflint
require_cmd helm
require_cmd kubeconform

K8S_VERSION="${K8S_VERSION:-1.28.0}"
CHART="${CHART:-deploy/k8s/chart/aero-gateway}"
CRD_SCHEMA_LOCATION="${CRD_SCHEMA_LOCATION:-https://raw.githubusercontent.com/datreeio/CRDs-catalog/main/{{.Group}}/{{.ResourceKind}}_{{.ResourceAPIVersion}}.json}"

echo "==> Deploy manifest hygiene (labels + docker compose config)"
node scripts/ci/check-deploy-manifests.mjs

echo "==> Security header templates (canonicalization)"
node scripts/ci/check-security-headers.mjs

echo "==> Terraform fmt"
terraform fmt -check -diff -recursive infra

# Mirror IaC CI behaviour: treat infra/<module>/ directories containing `.tf`
# as "root modules", and require a committed provider lockfile for each.
mapfile -t root_candidates < <(find infra -mindepth 2 -maxdepth 2 -type f -name '*.tf' -print0 | xargs -0 -r -n1 dirname | sort -u)
for module in "${root_candidates[@]}"; do
  if [[ ! -f "$module/.terraform.lock.hcl" ]]; then
    echo "error: Terraform module '$module' is missing .terraform.lock.hcl." >&2
    echo "Run: terraform -chdir=\"$module\" init (and commit the generated lockfile)." >&2
    exit 1
  fi
done

mapfile -t tf_modules < <(find infra -name '.terraform.lock.hcl' -print0 | xargs -0 -r -n1 dirname | sort -u)
if [[ ${#tf_modules[@]} -eq 0 ]]; then
  echo "error: no Terraform modules found under infra/ (expected at least one infra/**/.terraform.lock.hcl)" >&2
  exit 1
fi

for module in "${tf_modules[@]}"; do
  echo "==> Terraform init/validate ($module)"
  terraform -chdir="$module" init -backend=false -input=false -lockfile=readonly
  terraform -chdir="$module" validate -no-color

  echo "==> tflint ($module)"
  tflint --chdir="$module" --init
  tflint --chdir="$module" --format compact
done

values_files=(
  values-dev.yaml
  values-prod.yaml
  values-traefik.yaml
  values-prod-certmanager.yaml
  values-prod-certmanager-issuer.yaml
  values-prod-appheaders.yaml
)

for values in "${values_files[@]}"; do
  echo "==> Helm lint ($values)"
  helm lint "$CHART" --strict --kube-version "$K8S_VERSION" -f "$CHART/$values"

  echo "==> Helm template + kubeconform ($values)"
  out="/tmp/aero-gateway-${values%.yaml}.yaml"
  helm template aero-gateway "$CHART" -n aero --kube-version "$K8S_VERSION" -f "$CHART/$values" >"$out"

  kubeconform \
    -strict \
    -schema-location default \
    -schema-location "$CRD_SCHEMA_LOCATION" \
    -kubernetes-version "$K8S_VERSION" \
    -summary \
    "$out"

  # Ensure rendered output includes the canonical header values when ingress-level
  # injection is enabled. `values-prod-appheaders.yaml` intentionally disables
  # ingress injection and sets COOP/COEP at the app layer instead.
  if [[ "$values" != "values-prod-appheaders.yaml" ]]; then
    node - "$out" <<'NODE'
const fs = require('node:fs');

const outPath = process.argv[2];
const text = fs.readFileSync(outPath, 'utf8');

const headers = JSON.parse(fs.readFileSync('scripts/headers.json', 'utf8'));
const expected = { ...headers.crossOriginIsolation, ...headers.baseline, ...headers.contentSecurityPolicy };

const missing = [];
for (const [key, value] of Object.entries(expected)) {
  // nginx snippet form:
  if (text.includes(`add_header ${key} "${value}"`)) continue;
  // Traefik middleware YAML form:
  if (text.includes(`${key}: "${value}"`)) continue;
  missing.push(key);
}

if (missing.length !== 0) {
  console.error(`error: rendered Helm manifests missing canonical headers: ${missing.join(', ')}`);
  process.exit(1);
}
NODE
  fi
done

echo "==> kubeconform (raw manifests)"
kubeconform -strict -schema-location default -schema-location "$CRD_SCHEMA_LOCATION" -kubernetes-version "$K8S_VERSION" -summary deploy/k8s/aero-storage-server
kubeconform -strict -schema-location default -schema-location "$CRD_SCHEMA_LOCATION" -kubernetes-version "$K8S_VERSION" -summary deploy/k8s/examples/cert-manager

echo "All IaC checks passed."
