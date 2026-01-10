#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CERT_DIR="${ROOT_DIR}/.certs"

mkdir -p "${CERT_DIR}"

CERT_PATH="${CERT_DIR}/localhost.crt"
KEY_PATH="${CERT_DIR}/localhost.key"

OPENSSL_CONFIG="$(mktemp)"
trap 'rm -f "${OPENSSL_CONFIG}"' EXIT

cat >"${OPENSSL_CONFIG}" <<'EOF'
[req]
distinguished_name = req_distinguished_name
x509_extensions = v3_req
prompt = no

[req_distinguished_name]
CN = localhost

[v3_req]
subjectAltName = @alt_names

[alt_names]
DNS.1 = localhost
IP.1 = 127.0.0.1
EOF

openssl req \
  -x509 \
  -newkey rsa:2048 \
  -nodes \
  -sha256 \
  -days 3650 \
  -keyout "${KEY_PATH}" \
  -out "${CERT_PATH}" \
  -config "${OPENSSL_CONFIG}"

echo "Generated:"
echo "  ${CERT_PATH}"
echo "  ${KEY_PATH}"
echo
echo "Run the gateway with:"
echo "  TLS_ENABLED=1 TLS_CERT_PATH=\"${CERT_PATH}\" TLS_KEY_PATH=\"${KEY_PATH}\" npm start"
