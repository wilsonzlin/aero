/**
 * Extremely small YAML helper for contract tests.
 *
 * Extracts `run:` scripts from GitHub Action YAML files without bringing in a full YAML parser.
 * We only support the subset used in this repo:
 *
 * - `run: <single line>`
 * - `run: |` / `run: >` followed by an indented block scalar
 *
 * The output is the raw script content (block scalars de-indented to match the block baseline).
 */

/**
 * @param {string} line
 * @returns {number}
 */
function leadingSpaces(line) {
  let n = 0;
  while (n < line.length && line.charCodeAt(n) === 32) n++;
  return n;
}

/**
 * @param {string} yamlText
 * @returns {string[]}
 */
export function extractRunScripts(yamlText) {
  /** @type {string[]} */
  const scripts = [];
  const lines = yamlText.split("\n");
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const trimmed = line.trimStart();
    if (!trimmed.startsWith("run:")) continue;

    const baseIndent = leadingSpaces(line);
    const after = trimmed.slice("run:".length).trimStart();
    if (after.startsWith("|") || after.startsWith(">")) {
      const blockIndentMin = baseIndent + 1;
      const block = [];
      for (let j = i + 1; j < lines.length; j++) {
        const next = lines[j];
        if (!next.trim()) {
          block.push("");
          continue;
        }
        if (leadingSpaces(next) < blockIndentMin) break;
        block.push(next.slice(blockIndentMin));
        i = j;
      }
      scripts.push(block.join("\n"));
      continue;
    }

    scripts.push(after);
  }
  return scripts;
}

