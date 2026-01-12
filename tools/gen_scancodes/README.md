# Scancode table generation (`KeyboardEvent.code` → PS/2 Set 2)

The single source of truth for browser keyboard scancode translation is:

- `tools/gen_scancodes/scancodes.json`

This JSON is used to generate mapping tables consumed by both TypeScript and Rust
so the browser capture side and the PS/2 device model stay in sync.

## Generated outputs

Running the generator updates:

- `src/input/scancodes.ts`
- `web/src/input/scancodes.ts`
- `crates/aero-devices-input/src/scancodes_generated.rs`
- `crates/emulator/src/io/input/scancodes.rs` (legacy harness, if present)

## Regenerating

From the repo root:

```bash
npm run gen:scancodes
# or:
node tools/gen_scancodes/gen_scancodes.mjs
```

## Drift prevention (CI)

CI/unit tests validate that `scancodes.json` and the generated outputs do not
silently diverge:

- `web/src/input/scancodes_drift.test.ts` compares `scancodes.json` against the
  generated TypeScript mapping and checks that the `src/` and `web/` TS outputs
  are identical.
- `crates/aero-devices-input/tests/scancodes_json_parity.rs` compares
  `scancodes.json` against the generated Rust lookup and ensures the legacy
  emulator copy (if present) matches `aero-devices-input`.

If any of these tests fail, regenerate using the command above and commit the
updated generated files.

