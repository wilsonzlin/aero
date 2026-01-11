# Release process

This repository ships two primary production artifacts:

1. **Static web build** (`web/dist/`)
   - Includes both WASM variants (**threaded** + **single fallback**).
   - Includes hosting templates (`_headers`) for COOP/COEP + CSP + caching.
   - Includes provenance metadata (`/aero.version.json`) and shows the commit SHA in the UI.
2. **Backend gateway container image** (`backend/aero-gateway`)
   - Published to GHCR as `ghcr.io/<owner>/aero-gateway`.
   - Includes provenance metadata via OCI labels + `/version` response.

GitHub Actions produces these artifacts via:

- `.github/workflows/release-web.yml`
- `.github/workflows/release-gateway.yml`

---

## Cut a release (recommended)

1. Ensure `main` is green (CI passing).
2. Pick a semver tag (e.g. `v0.1.0`).
3. Create and push the tag:

   ```bash
   git checkout main
   git pull --rebase
   git tag -a v0.1.0 -m "v0.1.0"
   git push origin v0.1.0
   ```

4. Wait for GitHub Actions to finish:
   - **Release - web** uploads `aero-web-v0.1.0.zip` to the GitHub Release.
   - **Release - aero-gateway image** publishes `ghcr.io/<owner>/aero-gateway:0.1.0` (and related tags).

### Verify the release artifacts

- GitHub Release contains `aero-web-<tag>.zip`
  - Unzipping it should yield the contents of `web/dist/` (not a nested `dist/` directory).
  - Must contain:
    - `_headers` (COOP/COEP/CSP templates)
    - `aero.version.json` (build provenance)
    - at least two `.wasm` binaries (single + threaded)
- GHCR contains an image for the tag:
  - `ghcr.io/<owner>/aero-gateway:<tag>` (e.g. `v0.1.0`)
  - `ghcr.io/<owner>/aero-gateway:<version>` (e.g. `0.1.0`)

### Tagging / versioning behavior

#### Web artifact naming

`release-web.yml` names the uploaded zip using the Git ref:

- For tag releases: `aero-web-v0.1.0.zip`
- For manual runs: `aero-web-<ref>.zip` (unless you also provide the optional `tag` input)
  - Note: `/` characters in branch names are replaced with `-` so the artifact name is a single file (e.g. `feature/foo` → `aero-web-feature-foo.zip`).

#### Gateway image tags

`release-gateway.yml` publishes multiple tags for convenience:

- `v0.1.0` (raw git tag)
- `0.1.0`, `0.1`, `0` (semver forms)
- `sha-<short>` (always)
- `latest` **only on pushes to `main`** (default branch)

For non-tag publishes (e.g. the `latest` image built from `main`), the gateway embeds
`version: "sha-<short>"` in `GET /version` so the running container is self-describing.

---

## Web deployment (Netlify / Vercel / Cloudflare Pages)

See [`docs/deployment.md`](./deployment.md) for the full COOP/COEP + CSP background and hosting templates.

### Option A: Deploy from the repo (recommended)

This is the simplest option because platform-specific config files are applied automatically:

- Netlify: `netlify.toml` + `web/public/_headers`
- Vercel: `vercel.json`
- Cloudflare Pages: `web/public/_headers`

Configure your host with:

- Root directory: `.`
- Build command: `PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm ci && npm -w web run build`
- Output directory: `web/dist`

### Option B: Deploy a GitHub Release web artifact (zip)

The GitHub Release asset is a pre-built `web/dist` bundle. This is useful when you want to deploy without building
from source (e.g. air-gapped or pinned deploys).

1. Download `aero-web-<tag>.zip` from the GitHub Release.
2. Unzip it (it contains the static files directly).
3. Deploy the extracted directory as your static site root.

#### Netlify (manual deploy)

- Drag-and-drop the unzipped directory in the Netlify UI, or use the CLI:

  ```bash
  netlify deploy --prod --dir .
  ```

Netlify will apply the `_headers` file automatically.

#### Cloudflare Pages (Wrangler)

```bash
wrangler pages deploy . --project-name <your-project>
```

Cloudflare Pages will apply the `_headers` file automatically.

#### Vercel

Vercel does **not** use Netlify-style `_headers`. For Vercel deployments, prefer **Option A** (deploy from the repo)
so `vercel.json` is applied and COOP/COEP/CSP headers are correct.

If you must deploy the zip artifact to Vercel, you must configure the response headers yourself to match
`docs/security-headers.md` (COOP/COEP + CSP with `wasm-unsafe-eval`).

---

## Gateway image deployment (Docker / Kubernetes)

### Docker (single container)

```bash
docker run --rm -p 8080:8080 \
  -e PUBLIC_BASE_URL="http://localhost:8080" \
  ghcr.io/<owner>/aero-gateway:0.1.0
```

Check:

```bash
curl -fsS http://localhost:8080/healthz
curl -fsS http://localhost:8080/version
```

### docker-compose

```yaml
services:
  aero-gateway:
    image: ghcr.io/<owner>/aero-gateway:0.1.0
    ports:
      - "8080:8080"
    environment:
      PUBLIC_BASE_URL: "https://aero.example.com"
      # For reverse-proxy TLS termination setups:
      # TRUST_PROXY: "1"
```

### Kubernetes (Helm chart)

This repo includes a Helm chart at `deploy/k8s/chart/aero-gateway`:

```bash
helm upgrade --install aero-gateway ./deploy/k8s/chart/aero-gateway \
  --set gateway.image.repository=ghcr.io/<owner>/aero-gateway \
  --set gateway.image.tag=0.1.0
```

See `deploy/k8s/README.md` for ingress/TLS examples and COOP/COEP header strategies.

---

## Provenance metadata

### Web

- `GET /aero.version.json` is generated during the Vite build and includes:
  - `version` (tag or ref)
  - `gitSha`
  - `builtAt` (UTC ISO-8601)
- The UI renders the same information under **Build info**.

### Gateway

- `GET /version` returns:
  - `version`
  - `gitSha`
  - `builtAt`

---

## Reproducibility / pinned toolchains

Release workflows intentionally use the same pinned toolchain policy as CI:

- Node.js is pinned via the repo root [`.nvmrc`](../.nvmrc).
- Rust stable is pinned in [`rust-toolchain.toml`](../rust-toolchain.toml).
- The pinned nightly used for threaded WASM lives in [`scripts/toolchains.json`](../scripts/toolchains.json) (`rust.nightlyWasm`).
