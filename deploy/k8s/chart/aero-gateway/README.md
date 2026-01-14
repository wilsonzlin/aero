# aero-gateway Helm chart

This chart deploys the Aero backend gateway (`aero-gateway`) to Kubernetes.

Optionally (recommended for production networking), it can also deploy the L2 tunnel proxy
(`aero-l2-proxy`, ADR 0005 / “Option C”) and route `/l2` (and legacy alias `/eth`) to it behind
the same Ingress and security headers.

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
- `l2Proxy.enabled` / `l2Proxy.image.repository` / `l2Proxy.image.tag` (Option C)
- `l2Tunnel.maxFramePayloadBytes` / `l2Tunnel.maxControlPayloadBytes` (L2 tunnel payload limits)
- `l2Proxy.capture.dir` / `l2Proxy.capture.maxBytes` / `l2Proxy.capture.flushIntervalMs` (optional PCAPNG capture)
- `l2Proxy.extraVolumes` / `l2Proxy.extraVolumeMounts` (optional capture persistence / extra mounts)
- `ingress.host`
- `ingress.tls.enabled` / `ingress.tls.secretName` (or `certManager.enabled=true`)
- `ingress.coopCoep.enabled` (or `gateway.crossOriginIsolation.enabled=true`)
- `ingress.securityHeaders.enabled` / `ingress.securityHeaders.contentSecurityPolicy`
- `secrets.create` / `secrets.existingSecret`
- `redis.enabled`

## Option C (`aero-l2-proxy`) / `/l2` routing (legacy alias `/eth`)

Enable the L2 proxy:

```yaml
l2Proxy:
  enabled: true
  image:
    repository: ghcr.io/wilsonzlin/aero-l2-proxy
    tag: "<REPLACE_WITH_L2_PROXY_IMAGE_TAG>"
```

When `l2Proxy.enabled=true` and `ingress.enabled=true`, the chart's Ingress will include:

- `/l2` → `aero-l2-proxy` Service (port 8090)
- `/eth` → `aero-l2-proxy` Service (port 8090) (legacy alias)
- `/<everything-else>` → `aero-gateway` Service

Because the same Ingress resource is used, the existing COOP/COEP/CSP (or Traefik Middleware)
header injection strategy applies to `/l2` as well.

### Origin allowlist (`AERO_L2_ALLOWED_ORIGINS` / `ALLOWED_ORIGINS`)

`aero-l2-proxy` enforces an Origin allowlist for WebSocket upgrades.

The proxy accepts the allowlist via:

- `AERO_L2_ALLOWED_ORIGINS` (preferred/explicit; takes precedence when set), or
- `ALLOWED_ORIGINS` (shared with `aero-gateway` and `proxy/webrtc-udp-relay`) when
  `AERO_L2_ALLOWED_ORIGINS` is unset.

This chart sets the L2 proxy Origin allowlist automatically (as `ALLOWED_ORIGINS`) to match the
derived Ingress origin (`http(s)://<ingress.host>`), and you can add additional exact origins via:

```yaml
l2Proxy:
  extraAllowedOrigins:
    - "https://another-frontend.example.com"
```

### Payload limits (`AERO_L2_MAX_FRAME_PAYLOAD` / `AERO_L2_MAX_CONTROL_PAYLOAD`)

The L2 tunnel framing includes per-message payload limits (bytes).

- `aero-l2-proxy` enforces these limits at runtime via the `AERO_L2_MAX_*` env vars.
- `aero-gateway` surfaces the same values to clients via `POST /session` `limits.l2.*` so web clients can size
  frames and control messages correctly.

Configure via:

```yaml
l2Tunnel:
  # Defaults (recommended): 2048/256
  maxFramePayloadBytes: 2048
  maxControlPayloadBytes: 256
```

Values must be positive integers; `0` is treated as unset (defaults apply).

### Auth mode (`AERO_L2_AUTH_MODE`)

`aero-l2-proxy` can authenticate `/l2` WebSocket upgrades via `AERO_L2_AUTH_MODE`.

This chart defaults to `session` auth for `/l2` (matching the gateway’s cookie-backed session used by `/tcp`).

Supported values:

- `none`
- `session` (recommended for same-origin browser clients; legacy alias: `cookie`)
- `token` (legacy alias: `api_key`)
- `jwt`
- `cookie_or_jwt`
- `session_or_token` (legacy alias: `cookie_or_api_key`)
- `session_and_token` (legacy alias: `cookie_and_api_key`)

Example:

```yaml
l2Proxy:
  auth:
    mode: "session"
```

If you use `session` / `cookie_or_jwt` / `session_or_token` / `session_and_token`, the proxy must share the gateway
session signing secret (`SESSION_SECRET`) to verify gateway-issued cookies (see below).

### Session secret sharing (`session` / `cookie_or_jwt` / `session_or_token` / `session_and_token` auth)

If your `aero-l2-proxy` auth mode verifies gateway-issued session cookies (`AERO_L2_AUTH_MODE=session`,
`cookie_or_jwt`, `session_or_token`, or `session_and_token`), it must use the same signing secret as the gateway
(`SESSION_SECRET`).

By default, this chart reuses the gateway secret configured under `secrets.*` (so both pods see the
same `SESSION_SECRET` value). If you need to source `SESSION_SECRET` from a different Secret:

```yaml
l2Proxy:
  sessionSecret:
    existingSecret: "my-session-secret"
    key: "SESSION_SECRET"
```

The chart also sets `AERO_L2_SESSION_SECRET` to the same value for forward-compatibility.

### Optional: L2 tunnel capture (PCAPNG) (debugging only)

`aero-l2-proxy` can optionally write per-tunnel PCAPNG capture files for debugging.

**Privacy warning:** captures contain raw network traffic and may include sensitive user data (DNS queries,
plaintext protocols, credentials, etc.). Treat capture files as secrets and avoid enabling capture on
internet-exposed production deployments unless you have an explicit retention/privacy plan.

Enable capture by setting a writable directory inside the container:

```yaml
l2Proxy:
  capture:
    # Note: the container runs as non-root; prefer a path under /tmp (or mount a writable volume).
    # Capture files are written into the pod filesystem unless you mount something at this path.
    dir: /tmp/aero-l2-captures
    maxBytes: 67108864 # 64 MiB per capture file (includes PCAPNG overhead; 0 disables the cap)
    flushIntervalMs: 1000 # 0 disables periodic flushing (capture is flushed on close)
```

To persist captures (or to support `readOnlyRootFilesystem`), mount a volume at the capture path:

```yaml
l2Proxy:
  # The image runs as a non-root user (uid 10001). Depending on your storage class,
  # you may need to set fsGroup so the mounted volume is writable.
  podSecurityContext:
    fsGroup: 10001
  capture:
    dir: /captures
  extraVolumeMounts:
    - name: captures
      mountPath: /captures
  extraVolumes:
    - name: captures
      persistentVolumeClaim:
        claimName: my-capture-pvc
```

## Metrics / ServiceMonitor (optional)

`aero-l2-proxy` exposes Prometheus metrics at `GET /metrics` on port 8090.

If you use the Prometheus Operator, you can enable a `ServiceMonitor`:

```yaml
l2Proxy:
  serviceMonitor:
    enabled: true
```

## UDP relay (guest UDP)

This chart deploys `aero-gateway` (and optionally `aero-l2-proxy`). Guest UDP requires deploying the
separate relay service under:

- [`proxy/webrtc-udp-relay`](../../../../proxy/webrtc-udp-relay/)

To have the gateway return `udpRelay` connection metadata in `POST /session`, configure:

- `UDP_RELAY_BASE_URL` (accepts `http(s)://` or `ws(s)://`, plus optional auth settings like `UDP_RELAY_AUTH_MODE`, `UDP_RELAY_API_KEY`, `UDP_RELAY_JWT_SECRET`)

These can be provided via `config.data`, `gateway.extraEnv`, or an existing Secret referenced by the chart.

Inbound UDP filtering note: `proxy/webrtc-udp-relay` defaults to `UDP_INBOUND_FILTER_MODE=address_and_port` (only accept inbound UDP from remote address+port tuples the guest previously sent to). You can switch to full-cone behavior with `UDP_INBOUND_FILTER_MODE=any` (**less safe**) if required by your workload.

WebRTC DataChannel hardening note: `proxy/webrtc-udp-relay` configures pion/SCTP caps to mitigate oversized WebRTC DataChannel message DoS and to bound receive-side buffering/allocation before `DataChannel.OnMessage` runs. These are configurable on the relay via:

- `WEBRTC_DATACHANNEL_MAX_MESSAGE_BYTES` (SDP `a=max-message-size` hint; 0 = auto)
- `WEBRTC_SCTP_MAX_RECEIVE_BUFFER_BYTES` (hard receive-side cap; 0 = auto; must be ≥ `WEBRTC_DATACHANNEL_MAX_MESSAGE_BYTES` and ≥ `1500`)
- `WEBRTC_SESSION_CONNECT_TIMEOUT` (close server-side PeerConnections that never connect; default `30s`)

Security note: `GET /webrtc/ice` responses may include sensitive TURN credentials (especially TURN REST ephemeral creds) and are explicitly **non-cacheable** (`Cache-Control: no-store`, `Pragma: no-cache`, `Expires: 0`). Ensure your Ingress/proxy preserves these headers and does not inject caching headers for `/webrtc/ice`.

See also:

- [`docs/backend/01-aero-gateway-api.md`](../../../../docs/backend/01-aero-gateway-api.md)
