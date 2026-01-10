import { readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import wabtInit from 'wabt';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const repoRoot = path.resolve(__dirname, '..');
const watPath = path.join(repoRoot, 'web', 'public', 'wasm-jit-csp', 'mega-module.wat');
const wasmPath = path.join(repoRoot, 'web', 'public', 'wasm-jit-csp', 'mega-module.wasm');

const wabt = await wabtInit();
const wat = await readFile(watPath, 'utf8');

const module = wabt.parseWat(watPath, wat, { multi_memory: true });
const { buffer } = module.toBinary({ log: false, write_debug_names: true });

await writeFile(wasmPath, Buffer.from(buffer));
// eslint-disable-next-line no-console
console.log(`[generate] wrote ${path.relative(repoRoot, wasmPath)} (${buffer.length} bytes)`);
