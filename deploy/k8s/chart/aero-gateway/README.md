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
