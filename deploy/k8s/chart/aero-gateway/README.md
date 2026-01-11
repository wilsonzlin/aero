# aero-gateway Helm chart

This chart deploys the Aero backend gateway (`aero-gateway`) to Kubernetes.

For the complete walkthrough (TLS + COOP/COEP + WebSocket verification), see:

- [`deploy/k8s/README.md`](../../README.md)

## Quick install

```bash
helm upgrade --install aero-gateway ./deploy/k8s/chart/aero-gateway \
  -n aero --create-namespace \
  -f ./aero-values.yaml
```

## Key values

- `gateway.image.repository` / `gateway.image.tag`
- `ingress.host`
- `ingress.tls.enabled` / `ingress.tls.secretName` (or `certManager.enabled=true`)
- `ingress.coopCoep.enabled` (or `gateway.crossOriginIsolation.enabled=true`)
- `ingress.securityHeaders.enabled` / `ingress.securityHeaders.contentSecurityPolicy`
- `secrets.create` / `secrets.existingSecret`
- `redis.enabled`

## UDP relay (guest UDP)

This chart deploys **only** the gateway. Guest UDP requires deploying the separate relay service under:

- [`proxy/webrtc-udp-relay`](../../../../proxy/webrtc-udp-relay/)

To have the gateway return `udpRelay` connection metadata in `POST /session`, configure:

- `UDP_RELAY_BASE_URL` (and optional auth settings like `UDP_RELAY_AUTH_MODE`, `UDP_RELAY_API_KEY`, `UDP_RELAY_JWT_SECRET`)

These can be provided via `config.data`, `gateway.extraEnv`, or an existing Secret referenced by the chart.

See also:

- [`docs/backend/01-aero-gateway-api.md`](../../../../docs/backend/01-aero-gateway-api.md)
