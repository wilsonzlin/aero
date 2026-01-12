# Kubernetes deployment (aero-gateway)

This directory contains a minimal **Helm chart** to deploy the Aero backend gateway (`aero-gateway`) to Kubernetes.

Note: the gateway covers **TCP (WebSocket)** and **DNS-over-HTTPS**, but guest **UDP** requires deploying the separate relay service [`proxy/webrtc-udp-relay`](../../proxy/webrtc-udp-relay/) and configuring the gateway with `UDP_RELAY_BASE_URL` (accepts `http(s)://` or `ws(s)://`) so `POST /session` can return `udpRelay` connection metadata (see [`docs/backend/01-aero-gateway-api.md`](../../docs/backend/01-aero-gateway-api.md)).

The chart includes:

- `Deployment` + `Service` for `aero-gateway`
- `Ingress` example (defaults to `ingress-nginx`) that:
  - terminates TLS (optional in dev; recommended in prod)
  - supports WebSocket upgrades (Aero uses `wss://<host>/tcp?v=1&host=<dst>&port=<dstPort>`)
  - for the Option C L2 tunnel (`wss://<host>/l2`, subprotocol `aero-l2-tunnel-v1`), enable the
    optional `aero-l2-proxy` deployment (`l2Proxy.enabled=true`) so `/l2` is routed to it
  - can inject security headers (COOP/COEP/CORP/OAC + CSP) required for `SharedArrayBuffer` / `crossOriginIsolated`
- Optional in-cluster Redis (useful when running multiple gateway replicas)
- Optional `NetworkPolicy` template to help restrict ingress/egress

## L2 tunnel proxy (`/l2`)

If you are using the recommended Option C networking path (tunneling raw Ethernet frames over WebSocket),
you should deploy the L2 tunnel proxy:

- `aero-l2-proxy` (Rust service under `crates/aero-l2-proxy`)

This Helm chart can deploy it for you. Enable:

```yaml
l2Proxy:
  enabled: true
  image:
    repository: ghcr.io/<owner>/aero-l2-proxy
    tag: "<REPLACE_WITH_IMAGE_TAG>"
```

When enabled, the chart's Ingress routes:

- `/l2` → `aero-l2-proxy` Service (port 8090)
- everything else → `aero-gateway` Service

If you override the L2 tunnel payload limits (`AERO_L2_MAX_FRAME_PAYLOAD` / `AERO_L2_MAX_CONTROL_PAYLOAD`),
configure `l2Tunnel.maxFramePayloadBytes` / `l2Tunnel.maxControlPayloadBytes` so both `aero-l2-proxy` (enforcement)
and `aero-gateway` (advertising via `POST /session` `limits.l2`) stay in sync.

If you manage your Ingress separately, the equivalent nginx Ingress path addition looks like:

```yaml
paths:
  - path: /l2
    pathType: Prefix
    backend:
      service:
        name: aero-l2-proxy
        port:
          number: 8090
  - path: /
    pathType: Prefix
    backend:
      service:
        name: aero-gateway
        port:
          number: 80
```

## UDP relay service (`proxy/webrtc-udp-relay`)

This chart deploys **only** `aero-gateway`. To support guest UDP, deploy the relay service under
[`proxy/webrtc-udp-relay`](../../proxy/webrtc-udp-relay/) separately and configure the gateway with
`UDP_RELAY_BASE_URL` (accepts `http(s)://` or `ws(s)://`) so `POST /session` can return `udpRelay` connection metadata.

If you want a **single-origin** deployment (recommended), configure your Ingress to route the relay's HTTP/WebSocket endpoints to the relay Service:

```yaml
paths:
  - path: /webrtc
    pathType: Prefix
    backend:
      service:
        name: aero-webrtc-udp-relay
        port:
          number: 8080
  - path: /udp
    pathType: Prefix
    backend:
      service:
        name: aero-webrtc-udp-relay
        port:
          number: 8080
  - path: /offer
    pathType: Prefix
    backend:
      service:
        name: aero-webrtc-udp-relay
        port:
          number: 8080
  - path: /
    pathType: Prefix
    backend:
      service:
        name: aero-gateway
        port:
          number: 80
```

Important: WebRTC uses a **UDP port range** for ICE candidates and relay traffic; this cannot be reverse-proxied by an HTTP Ingress. You must publish/open the relay's UDP port range separately (e.g. a `LoadBalancer`/`NodePort` `Service` with UDP ports, or host networking) and configure the relay to match (see the relay README).

## Prerequisites

- Kubernetes v1.22+ (Ingress v1)
- Helm v3
- An Ingress controller (example install commands below):
  - `ingress-nginx` (default; uses `configuration-snippet` to add COOP/COEP + CSP headers), or
  - Traefik (supported via `Middleware` when `ingress.coopCoep.mode=traefik`)
- A DNS name pointing at your Ingress load balancer
- TLS certificate:
  - via cert-manager (recommended), or
  - manually created `Secret` of type `kubernetes.io/tls`

## CI validation (Helm + Kubernetes schema)

This repository's CI validates that the Helm chart renders correctly and produces valid Kubernetes objects.

To reproduce locally (requires `helm` + `kubeconform`; or run the repo-wide `bash ./scripts/ci/check-iac.sh` / `just check-iac`):

```bash
CHART=deploy/k8s/chart/aero-gateway

for values in \
  values-dev.yaml \
  values-prod.yaml \
  values-prod-with-l2.yaml \
  values-traefik.yaml \
  values-prod-certmanager.yaml \
  values-prod-certmanager-issuer.yaml \
  values-prod-appheaders.yaml; do
  helm lint "$CHART" --strict --kube-version 1.28.0 -f "$CHART/$values"
done

for values in \
  values-dev.yaml \
  values-prod.yaml \
  values-prod-with-l2.yaml \
  values-traefik.yaml \
  values-prod-certmanager.yaml \
  values-prod-certmanager-issuer.yaml \
  values-prod-appheaders.yaml; do
  out="/tmp/aero-${values%.yaml}.yaml"
  helm template aero-gateway "$CHART" -n aero --kube-version 1.28.0 -f "$CHART/$values" > "$out"

  kubeconform -strict \
    -schema-location default \
    -schema-location "https://raw.githubusercontent.com/datreeio/CRDs-catalog/main/{{.Group}}/{{.ResourceKind}}_{{.ResourceAPIVersion}}.json" \
    -kubernetes-version 1.28.0 -summary "$out"
done

# Validate the non-Helm manifests in this repo too:
kubeconform -strict \
  -schema-location default \
  -schema-location "https://raw.githubusercontent.com/datreeio/CRDs-catalog/main/{{.Group}}/{{.ResourceKind}}_{{.ResourceAPIVersion}}.json" \
  -kubernetes-version 1.28.0 -summary deploy/k8s/aero-storage-server
```

## Optional: install ingress-nginx (example)

If you don’t already have an Ingress controller, you can install ingress-nginx with Helm:

```bash
helm repo add ingress-nginx https://kubernetes.github.io/ingress-nginx
helm repo update

helm upgrade --install ingress-nginx ingress-nginx/ingress-nginx \
  -n ingress-nginx --create-namespace
```

If you plan to use nginx mode with ingress-level header injection (`ingress.coopCoep.enabled=true` or `ingress.securityHeaders.enabled=true`),
you may need to allow snippet annotations:

```bash
helm upgrade --install ingress-nginx ingress-nginx/ingress-nginx \
  -n ingress-nginx --create-namespace \
  --set controller.allowSnippetAnnotations=true
```

> Snippet annotations are a cluster-level security decision; consider using Traefik mode or app-level headers if you prefer to keep them disabled.

## Optional: install cert-manager (example)

If you want automated TLS (`certManager.enabled=true`), install cert-manager:

```bash
helm repo add jetstack https://charts.jetstack.io
helm repo update

helm upgrade --install cert-manager jetstack/cert-manager \
  -n cert-manager --create-namespace \
  --set crds.enabled=true
```

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

Alternatively, if you have **cert-manager** installed, you can have the chart
create a `Certificate` resource and skip manual TLS secret creation (see the
`values-prod-certmanager.yaml` example).

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

# If your ingress controller does not allow header snippet annotations, you can
# alternatively enable COOP/COEP at the application layer and disable ingress injection:
# gateway:
#   crossOriginIsolation:
#     enabled: true
# ingress:
#   coopCoep:
#     enabled: false
#   securityHeaders:
#     enabled: false
#
# Recommended for multi-replica deployments (session storage, rate limiting, etc.)
redis:
  enabled: true
EOF
```

### Using an existing Secret (recommended for real production)

In real clusters you will often manage secrets via:

- ExternalSecrets Operator
- SealedSecrets
- SOPS
- your CI/CD system

In that case, create the Secret separately (must contain keys like `SESSION_SECRET`, and any other gateway env vars you want to keep secret) and configure Helm to use it:

```bash
helm upgrade --install aero-gateway ./deploy/k8s/chart/aero-gateway \
  -n aero \
  --set secrets.create=false \
  --set secrets.existingSecret=aero-gateway-secrets \
  -f ./aero-values.yaml
```

### Using an existing ConfigMap (optional)

If you already manage non-secret configuration via a `ConfigMap`, set:

- `config.create=false`
- `config.name=<existing-configmap-name>`

The chart will reference it via `envFrom`.

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

## Example values files

The chart ships example values you can copy locally:

- `deploy/k8s/chart/aero-gateway/values-dev.yaml` – basic dev defaults
- `deploy/k8s/chart/aero-gateway/values-prod.yaml` – production-ish defaults (TLS + 2 replicas)
- `deploy/k8s/chart/aero-gateway/values-prod-with-l2.yaml` – production-ish defaults with the L2 proxy enabled
- `deploy/k8s/chart/aero-gateway/values-traefik.yaml` – Traefik Ingress + Middleware headers
- `deploy/k8s/chart/aero-gateway/values-prod-appheaders.yaml` – production with app-level COOP/COEP (no ingress snippets)
- `deploy/k8s/chart/aero-gateway/values-prod-certmanager.yaml` – production with cert-manager-managed TLS
- `deploy/k8s/chart/aero-gateway/values-prod-certmanager-issuer.yaml` – production with cert-manager TLS + chart-managed Issuer

## TLS with cert-manager (optional)

If cert-manager is available in your cluster, you can automate certificate issuance.

### Option A: use an existing ClusterIssuer/Issuer

1) Ensure a `ClusterIssuer` (or namespace-scoped `Issuer`) exists (example names: `letsencrypt-prod`, `letsencrypt-staging`).
2) Set (see `values-prod-certmanager.yaml`):
   - `ingress.tls.enabled=true`
   - `certManager.enabled=true`
   - `certManager.issuerRef.kind/name` appropriately

The chart will create a `Certificate` resource and use a deterministic TLS secret name
`<release>-aero-gateway-tls` unless you set `ingress.tls.secretName` explicitly.

Example `ClusterIssuer` manifests are provided under:

- `deploy/k8s/examples/cert-manager/clusterissuer-letsencrypt-staging.yaml`
- `deploy/k8s/examples/cert-manager/clusterissuer-letsencrypt-prod.yaml`

### Option B: have the chart create a namespace-scoped Issuer

If you prefer a single `helm install` (no separate Issuer/ClusterIssuer YAML), set (see `values-prod-certmanager-issuer.yaml`):

- `certManager.enabled=true`
- `certManager.createIssuer=true`
- `certManager.issuer.email=<your-email>` (required)

## Gateway config notes (PUBLIC_BASE_URL / origin allowlist)

`aero-gateway` enforces an `Origin` allowlist for browser-initiated requests.

This chart sets `PUBLIC_BASE_URL` automatically when `ingress.enabled=true` (derived from `ingress.host` and whether TLS is enabled).

If you are deploying **without** an Ingress, or the externally reachable URL is different from `ingress.host`, set:

- `gateway.publicBaseUrl=https://<your-domain>` (or `http://...` in dev)
- (Optional) `gateway.allowedOrigins=https://<your-frontend-origin>` (comma-separated)

If you see `403 Origin not allowed` responses, `PUBLIC_BASE_URL`/`ALLOWED_ORIGINS` is the first thing to check.

### TRUST_PROXY (behind an Ingress)

When running behind an Ingress (TLS terminated at the edge), `aero-gateway` should generally run with `TRUST_PROXY=1`
so it can trust `X-Forwarded-*` headers for scheme/client IP.

This chart defaults `TRUST_PROXY` to:

- `1` when `ingress.enabled=true`
- `0` when `ingress.enabled=false`

If you expose the `Service` directly to untrusted clients, keep `TRUST_PROXY=0` to avoid header spoofing.

### Graceful shutdown (terminationGracePeriodSeconds)

The gateway supports graceful shutdown via `SIGTERM` and uses `SHUTDOWN_GRACE_MS`.

This chart sets `terminationGracePeriodSeconds` automatically based on `gateway.shutdownGraceMs`
(with a small buffer) so Kubernetes does not force-kill the pod mid-shutdown. You can override it via:

```yaml
gateway:
  terminationGracePeriodSeconds: 90
```

## Private registry / imagePullSecrets (optional)

If your gateway image is in a private registry, set `imagePullSecrets` in your values file:

```yaml
imagePullSecrets:
  - name: regcred
```

## Service account token (security hardening)

By default, the chart disables automounting the Kubernetes service account token into the gateway pod
(`serviceAccount.automountServiceAccountToken=false`). This reduces blast radius if the container is compromised.

If you need in-cluster identity (rare for this service), set:

```yaml
serviceAccount:
  automountServiceAccountToken: true
```

## Autoscaling (HPA) (optional)

If you have `metrics-server` installed, you can enable a HorizontalPodAutoscaler:

```yaml
autoscaling:
  enabled: true
  minReplicas: 2
  maxReplicas: 10
  targetCPUUtilizationPercentage: 80
```

When autoscaling is enabled, the chart sets the Deployment replica count to `autoscaling.minReplicas`
and the HPA will manage scaling from there.

## Prometheus ServiceMonitor (optional)

If you use the Prometheus Operator (kube-prometheus-stack), you can enable a `ServiceMonitor`
to scrape `GET /metrics`:

```yaml
serviceMonitor:
  enabled: true
```

This requires the `monitoring.coreos.com/v1` CRDs to be installed in your cluster.

## Verify the rollout

```bash
kubectl -n aero get pods,svc,ingress
kubectl -n aero describe ingress aero-gateway
```

## Helm test (optional)

The chart includes a basic Helm test pod that hits `/healthz` and `/readyz` through the ClusterIP service:

```bash
helm test aero-gateway -n aero
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

`wss://<host>/tcp?v=1&host=<dst>&port=<dstPort>`

You can verify the **WebSocket handshake** with `websocat`:

```bash
websocat -v 'wss://aero.example.com/tcp?v=1&host=example.com&port=80'
```

You should see an HTTP 101 Switching Protocols response.

If your gateway enforces authentication (e.g. requires a session cookie from `POST /session`),
you may see `401 Unauthorized` instead. In that case, first obtain a cookie using your
gateway's session endpoint, then retry with a WebSocket client that can send custom
headers (e.g. `wscat -H 'Cookie: ...'`).

## Ingress notes (security header injection)

### ingress-nginx

The default configuration injects headers using the annotation:

`nginx.ingress.kubernetes.io/configuration-snippet`

Newer `ingress-nginx` releases may disable snippet annotations by default (for security hardening). If your controller rejects the Ingress, you have three options:

1. Enable snippet annotations in the controller (cluster-level decision).
2. Switch to Traefik and use a `Middleware` for headers (see below).
3. Disable ingress-level header injection and rely on application/edge headers instead:
   - `gateway.crossOriginIsolation.enabled=true`
   - `ingress.coopCoep.enabled=false`
   - `ingress.securityHeaders.enabled=false` (CSP must be set by your edge proxy / frontend hosting layer)

### Traefik (alternative)

Set:

- `ingress.className=traefik`
- `ingress.coopCoep.mode=traefik`

The chart will create a `Middleware` that sets COOP/COEP + CSP headers (when enabled) and wire it to the Ingress.

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

Ingress behavior:

- The NetworkPolicy always allows traffic from **pods in the same namespace** (use a dedicated namespace per app).
- To allow traffic from an Ingress controller running in a different namespace, add that namespace to `networkPolicy.ingress.allowedNamespaces` (defaults to `ingress-nginx`).

### Example: allow public internet, block private ranges

If your CNI supports `NetworkPolicy` and you want to reduce SSRF blast radius, you can allow `0.0.0.0/0` but exclude private and special-use ranges.

Create a local values file snippet like:

```yaml
networkPolicy:
  enabled: true
  egress:
    allowedCIDRs:
      - 0.0.0.0/0
    exceptCIDRs:
      - 10.0.0.0/8
      - 172.16.0.0/12
      - 192.168.0.0/16
      - 127.0.0.0/8
      - 169.254.0.0/16
```

Note: You may need to also exclude your cluster Pod/Service CIDRs if they are routable from the gateway nodes.
