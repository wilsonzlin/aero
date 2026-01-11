# Security (contributor guide)

This document complements [`SECURITY.md`](../SECURITY.md). It focuses on **everyday security hygiene** for contributors: handling secrets, responding to leaks, and triaging automated security findings.

## Secrets (local development)

### `.env` files

- **Never commit real secrets** (API keys, JWT signing keys, OAuth secrets, database URLs with passwords, etc).
- Use `*.env.example` files as **templates** with placeholder values.
  - Copy: `cp .env.example .env` (or `cp path/to/.env.example path/to/.env`)
  - Fill in values locally.
- `.env` files are gitignored by default.
  - If you need a new environment variable, update the relevant `*.env.example` file and any docs/scripts that reference it.

### Other common secret files

Avoid committing credentials in any form, including:

- `.npmrc` (can contain registry tokens)
- `.netrc` (contains machine credentials)
- `*.pem` / `*.key` / `*.p12` / `*.pfx` (private keys / cert bundles)
- `terraform.tfvars` / `*.tfvars` (frequently contains credentials)

If you genuinely need a certificate/key for tests, it must be a **non-sensitive fixture** and should be clearly documented as such.

## If a secret is leaked

Treat any leak as compromised.

1. **Rotate immediately**
   - Revoke/rotate the token, password, or key in the upstream system (GitHub, cloud provider, IdP, etc).
   - Assume the secret has been harvested if it was pushed to a public branch.
2. **Assess blast radius**
   - Identify what the secret could access (scopes/permissions).
   - Review audit logs if available.
3. **Remove it from history (optional but recommended)**
   - Rotation is the priority; history rewrite is secondary.
   - If required, use `git filter-repo` (preferred) or BFG to purge the secret, then force-push to affected branches.
4. **Notify maintainers privately**
   - Use the reporting process in [`SECURITY.md`](../SECURITY.md).
   - Include: what leaked, when, what was rotated, and any follow-up actions.

## Automated scanning (CodeQL + secret scanning)

### CodeQL (static analysis)

The workflow `.github/workflows/codeql.yml` runs CodeQL for:

- Rust (workspace)
- JavaScript/TypeScript (repo sources; dependencies installed from the canonical Node workspace)
- Go (`proxy/webrtc-udp-relay`)

Runs:

- weekly on a schedule
- on pull requests that touch Rust/TS/Go paths

Results are uploaded to **GitHub → Security → Code scanning alerts**.

Query selection and exclusions live in [`.github/codeql/codeql-config.yml`](../.github/codeql/codeql-config.yml). It currently uses `security-and-quality` and excludes low-precision queries to keep initial alert noise manageable.

### Triage process

When an alert is filed:

1. **Reproduce/understand** the finding (read the CodeQL query help and the trace).
2. Prefer **fixing** the issue in code.
3. If it is a false positive / acceptable risk:
   - **Dismiss** the alert in GitHub with a clear justification and (ideally) link to an issue.
   - Or add a **targeted suppression** with justification:
      - Prefer inline suppressions close to the code (so the rationale lives with the code).
      - Use the `codeql[...]` suppression comment format supported by the language, e.g.
        - `// codeql[javascript/<rule-id>] <justification>`
        - `// codeql[rust/<rule-id>] <justification>`
        - `// codeql[go/<rule-id>] <justification>`

Avoid broad suppressions (like disabling an entire query suite) unless there is a documented, reviewed reason.

### Secret scanning

If GitHub Secret Scanning is enabled for this repo, treat any alert as high priority:

- rotate/revoke first
- then clean up the repo/history as needed
- document what happened so we don’t repeat it
