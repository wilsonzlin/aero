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

The Rust `crates/emulator/` harness consumes the mapping via the shared
`aero-devices-input` crate (there is no longer a separate generated copy under
`crates/emulator/`).

## Regenerating

From the repo root:

```bash
npm run gen:scancodes
# or:
just gen-scancodes
# or:
node tools/gen_scancodes/gen_scancodes.mjs
```

## Drift prevention (CI)

CI/unit tests validate that `scancodes.json` and the generated outputs do not
silently diverge:

- `node tools/gen_scancodes/check_generated.mjs` runs the generator and fails if
  `git diff` shows the checked-in generated files are out of date.
  - Convenience alias: `npm run check:scancodes`
  - Convenience alias: `just check-scancodes`
- `web/test/scancodes_generated_sync.test.ts` compares `scancodes.json` against
  the generated TypeScript mappings in both `web/src/input/scancodes.ts` and
  `src/input/scancodes.ts`.
- `crates/aero-devices-input/tests/scancodes_json_sync.rs` compares
  `scancodes.json` against the generated Rust lookup (`browser_code_to_set2_bytes`).
- `crates/emulator/tests/scancodes_json_sync.rs` performs the same check for the
  emulator crate (which re-exports the shared mapping from `aero-devices-input`).

If any of these tests fail, regenerate using the command above and commit the
updated generated files.
