/**
 * Creates a minimal HUD panel that displays current hotspots.
 *
 * The caller is responsible for positioning/styling the returned element.
 *
 * @param {{
 *   perf: { export(): { hotspots: Array<{ pc: string, percent_of_total: number, hits: number, instructions: number }> } },
 *   topN?: number,
 *   refreshMs?: number,
 * }} options
 * @returns {HTMLElement}
 */
export function createHotspotsPanel(options) {
  const { perf, topN = 10, refreshMs = 500 } = options;

  const root = document.createElement('div');
  root.className = 'aero-hud-panel aero-hud-hotspots';

  const title = document.createElement('div');
  title.className = 'aero-hud-title';
  title.textContent = 'Hotspots';
  root.appendChild(title);

  const table = document.createElement('table');
  table.className = 'aero-hud-table';
  root.appendChild(table);

  function render() {
    const { hotspots } = perf.export();
    const rows = hotspots.slice(0, topN);

    table.replaceChildren();

    const header = document.createElement('tr');
    for (const label of ['PC', '%', 'hits', 'instr']) {
      const th = document.createElement('th');
      th.textContent = label;
      header.appendChild(th);
    }
    table.appendChild(header);

    for (const h of rows) {
      const tr = document.createElement('tr');
      const percent = Number.isFinite(h.percent_of_total) ? h.percent_of_total : 0;

      const cells = [
        typeof h.pc === 'string' ? h.pc : '',
        percent.toFixed(2),
        Number.isFinite(h.hits) ? String(h.hits) : '0',
        Number.isFinite(h.instructions) ? String(h.instructions) : '0',
      ];
      for (const value of cells) {
        const td = document.createElement('td');
        td.textContent = value;
        tr.appendChild(td);
      }
      table.appendChild(tr);
    }
  }

  render();
  const timer = setInterval(render, refreshMs);
  timer.unref?.();
  // Allow callers to stop updates by removing the element.
  root.addEventListener(
    'DOMNodeRemoved',
    () => {
      clearInterval(timer);
    },
    { once: true },
  );

  return root;
}
