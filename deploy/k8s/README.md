# Kubernetes deployment (aero-gateway)

This directory contains a minimal **Helm chart** to deploy the Aero backend gateway (`aero-gateway`) to Kubernetes.

The chart includes:

- `Deployment` + `Service` for `aero-gateway`
- `Ingress` example (defaults to `ingress-nginx`) that:
  - terminates TLS (optional in dev; recommended in prod)
  - supports WebSocket upgrades (Aero uses `wss://<host>/tcp?...`)
  - injects COOP/COEP headers required for `SharedArrayBuffer` / `crossOriginIsolated`
- Optional in-cluster Redis (useful when running multiple gateway replicas)
- Optional `NetworkPolicy` template to help restrict ingress/egress

## Prerequisites

- Kubernetes v1.22+ (Ingress v1)
- Helm v3
- An Ingress controller:
  - `ingress-nginx` (default; uses `configuration-snippet` to add COOP/COEP headers), or
  - Traefik (supported via `Middleware` when `ingress.coopCoep.mode=traefik`)
- A DNS name pointing at your Ingress load balancer
- TLS certificate:
  - via cert-manager (recommended), or
  - manually created `Secret` of type `kubernetes.io/tls`

## Quickstart (production)

1) Create a namespace:

```bash
kubectl create namespace aero
```

2) Create a TLS secret (example for a manually managed cert):

```bash
kubectl -n aero create secret tls aero-tls \
  --cert=./cert.pem \
  --key=./key.pem
```

3) Create a values file (do **not** commit this file; it contains secrets):

```bash
cat > ./aero-values.yaml <<'EOF'
gateway:
  image:
    repository: ghcr.io/wilsonzlin/aero-gateway
    tag: "<REPLACE_WITH_IMAGE_TAG>"

ingress:
  host: "aero.example.com"
  tls:
    enabled: true
    secretName: aero-tls

secrets:
  # Strongly recommended: generate randomly (e.g. `openssl rand -hex 32`)
  data:
    SESSION_SECRET: "<REPLACE_WITH_RANDOM_SECRET>"
    ADMIN_API_KEY: "<REPLACE_WITH_RANDOM_KEY>"

# Recommended for multi-replica deployments (session storage, rate limiting, etc.)
redis:
  enabled: true
EOF
```

4) Install / upgrade:

```bash
helm upgrade --install aero-gateway ./deploy/k8s/chart/aero-gateway \
  -n aero \
  -f ./aero-values.yaml
```

## Quickstart (dev)

For local clusters (kind/minikube), you can disable TLS and use plain HTTP:

```bash
helm upgrade --install aero-gateway ./deploy/k8s/chart/aero-gateway \
  -n aero --create-namespace \
  -f ./deploy/k8s/chart/aero-gateway/values-dev.yaml
```

Note: `SharedArrayBuffer` / `crossOriginIsolated` requires HTTPS + COOP/COEP, so a non-TLS dev setup is useful for iteration but not browser-threaded production mode.

## Verify the rollout

```bash
kubectl -n aero get pods,svc,ingress
kubectl -n aero describe ingress aero-gateway
```

## Verify COOP/COEP headers

These headers are required for `SharedArrayBuffer` (Chrome/Edge/Firefox):

- `Cross-Origin-Opener-Policy: same-origin`
- `Cross-Origin-Embedder-Policy: require-corp`

Verify from a machine that can reach your Ingress:

```bash
curl -sI https://aero.example.com/ | egrep -i \
  'cross-origin-(opener|embedder|resource)-policy|origin-agent-cluster'
```

Browser-side verification:

1. Open `https://aero.example.com/`
2. DevTools console:
   - `crossOriginIsolated` should be `true`
   - `typeof SharedArrayBuffer` should be `"function"`

## Verify WebSocket (`/tcp`) connectivity

The emulator connects to:

`wss://<host>/tcp?host=<dst>&port=<dstPort>`

You can verify the **WebSocket handshake** with `websocat`:

```bash
websocat -v 'wss://aero.example.com/tcp?host=example.com&port=80'
```

You should see an HTTP 101 Switching Protocols response.

## Ingress notes (COOP/COEP header injection)

### ingress-nginx

The default configuration injects headers using the annotation:

`nginx.ingress.kubernetes.io/configuration-snippet`

Newer `ingress-nginx` releases may disable snippet annotations by default (for security hardening). If your controller rejects the Ingress, you have three options:

1. Enable snippet annotations in the controller (cluster-level decision).
2. Switch to Traefik and use a `Middleware` for headers (see below).
3. Inject COOP/COEP headers directly in `aero-gateway` (application-level).

### Traefik (alternative)

Set:

- `ingress.className=traefik`
- `ingress.coopCoep.mode=traefik`

The chart will create a `Middleware` that sets COOP/COEP headers and wire it to the Ingress.

## Network policy (recommended)

If your CNI supports `NetworkPolicy`, you can enable the included template:

```bash
helm upgrade --install aero-gateway ./deploy/k8s/chart/aero-gateway \
  -n aero \
  -f ./aero-values.yaml \
  --set networkPolicy.enabled=true
```

Egress is highly environment-specific:

- If `aero-gateway` needs to open arbitrary outbound TCP connections (typical for a TCP proxy), you may need to allow broad egress.
- If you can constrain destination IP ranges/ports, populate `networkPolicy.egress.allowedCIDRs` accordingly.

