## Golden images

This directory contains committed PNG "goldens" used by Playwright graphics regression tests (see
`tests/e2e/playwright/gpu_golden.spec.ts`).

Some of these images are generated from deterministic, CPU-rendered scenes. To regenerate them:

```bash
npm ci
npm run generate:goldens
```

CI enforces that the committed goldens stay in sync with the generator output. If CI fails with a
`tests/golden` diff, rerun `npm run generate:goldens` and commit the updated PNGs.

