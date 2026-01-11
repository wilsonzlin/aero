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
  helm lint "$CHART" --kube-version "$K8S_VERSION" -f "$CHART/$values"

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
done

echo "==> kubeconform (raw manifests)"
kubeconform -strict -schema-location default -schema-location "$CRD_SCHEMA_LOCATION" -kubernetes-version "$K8S_VERSION" -summary deploy/k8s/aero-storage-server
kubeconform -strict -schema-location default -schema-location "$CRD_SCHEMA_LOCATION" -kubernetes-version "$K8S_VERSION" -summary deploy/k8s/examples/cert-manager

echo "All IaC checks passed."

