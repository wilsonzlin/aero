/* global document, fetch */

function byId(id) {
  const el = document.getElementById(id);
  if (!el) throw new Error(`Missing element #${id}`);
  return el;
}

function prettyName(name) {
  return name.replace(/_/g, " ");
}

function sortEntries(history) {
  const entries = Object.values(history.entries ?? {});
  entries.sort((a, b) => String(a.timestamp).localeCompare(String(b.timestamp)));
  return entries;
}

function formatValue(value) {
  if (!Number.isFinite(value)) return String(value);
  if (Math.abs(value) >= 1000) return value.toFixed(0);
  if (Math.abs(value) >= 10) return value.toFixed(2);
  return value.toFixed(3);
}

function classifyDelta(prev, next, better, unit) {
  if (!Number.isFinite(prev) || !Number.isFinite(next)) return { className: "neutral", text: "—" };
  const delta = next - prev;
  const pct = prev === 0 ? null : (delta / prev) * 100;

  let improved = false;
  if (better === "lower") improved = delta < 0;
  if (better === "higher") improved = delta > 0;

  const className = delta === 0 ? "neutral" : improved ? "improvement" : "regression";
  const sign = delta > 0 ? "+" : "";
  const pctText = pct === null ? "" : ` (${pct > 0 ? "+" : ""}${pct.toFixed(2)}%)`.replace("+-", "-");
  const unitText = unit ? ` ${unit}` : "";
  return { className, text: `${sign}${formatValue(delta)}${unitText}${pctText}`.replace("+-", "-") };
}

function createSvgLineChart(points, { width = 840, height = 220, padding = 36 } = {}) {
  const svgNS = "http://www.w3.org/2000/svg";

  const svg = document.createElementNS(svgNS, "svg");
  svg.setAttribute("viewBox", `0 0 ${width} ${height}`);
  svg.setAttribute("class", "chart");

  if (points.length === 0) return svg;

  const values = points.map((p) => p.y);
  let minY = Math.min(...values);
  let maxY = Math.max(...values);

  if (minY === maxY) {
    minY -= 1;
    maxY += 1;
  }

  const range = maxY - minY;
  const plotW = width - padding * 2;
  const plotH = height - padding * 2;

  const xForIndex = (i) => padding + (i / Math.max(1, points.length - 1)) * plotW;
  const yForValue = (y) => padding + (1 - (y - minY) / range) * plotH;

  const axes = document.createElementNS(svgNS, "path");
  axes.setAttribute(
    "d",
    `M${padding},${padding} L${padding},${height - padding} L${width - padding},${height - padding}`,
  );
  axes.setAttribute("class", "chart-axes");
  svg.appendChild(axes);

  const line = document.createElementNS(svgNS, "polyline");
  line.setAttribute(
    "points",
    points.map((p, idx) => `${xForIndex(idx)},${yForValue(p.y)}`).join(" "),
  );
  line.setAttribute("class", "chart-line");
  svg.appendChild(line);

  const labelMin = document.createElementNS(svgNS, "text");
  labelMin.textContent = formatValue(minY);
  labelMin.setAttribute("x", String(padding));
  labelMin.setAttribute("y", String(height - padding + 16));
  labelMin.setAttribute("class", "chart-label");
  svg.appendChild(labelMin);

  const labelMax = document.createElementNS(svgNS, "text");
  labelMax.textContent = formatValue(maxY);
  labelMax.setAttribute("x", String(padding));
  labelMax.setAttribute("y", String(padding - 8));
  labelMax.setAttribute("class", "chart-label");
  svg.appendChild(labelMax);

  for (let i = 0; i < points.length; i++) {
    const point = points[i];
    const cx = xForIndex(i);
    const cy = yForValue(point.y);

    const dot = document.createElementNS(svgNS, "circle");
    dot.setAttribute("cx", String(cx));
    dot.setAttribute("cy", String(cy));
    dot.setAttribute("r", "4");
    dot.setAttribute("class", "chart-point");

    const title = document.createElementNS(svgNS, "title");
    const extras = [];
    if (Number.isFinite(point.n)) extras.push(`n=${point.n}`);
    if (Number.isFinite(point.cv)) extras.push(`CV ${(point.cv * 100).toFixed(2)}%`);
    title.textContent = `${point.timestamp} • ${point.commit} • ${formatValue(point.y)} ${point.unit}${extras.length ? ` • ${extras.join(" • ")}` : ""}`;
    dot.appendChild(title);

    if (point.url) {
      const link = document.createElementNS(svgNS, "a");
      link.setAttribute("href", point.url);
      link.setAttribute("target", "_blank");
      link.setAttribute("rel", "noreferrer");
      link.appendChild(dot);
      svg.appendChild(link);
    } else {
      svg.appendChild(dot);
    }
  }

  return svg;
}

function renderDashboard(history) {
  const entries = sortEntries(history);
  const status = byId("status");
  const summary = byId("summary");
  const metricsContainer = byId("metrics");
  const runsSection = byId("runs-section");

  status.hidden = true;
  summary.hidden = false;
  metricsContainer.hidden = false;
  runsSection.hidden = false;

  if (entries.length === 0) {
    summary.textContent = "No benchmark history recorded yet.";
    return;
  }

  const latest = entries[entries.length - 1];
  const env = latest.environment ?? {};
  const envParts = [];
  if (env.chromiumVersion) envParts.push(`Chromium ${env.chromiumVersion}`);
  if (env.node) envParts.push(`Node ${env.node}`);
  if (env.platform) {
    envParts.push(env.osRelease ? `${env.platform} ${env.osRelease}` : env.platform);
  }
  if (env.arch) envParts.push(env.arch);
  if (env.cpuModel) envParts.push(env.cpuCount ? `${env.cpuModel} (${env.cpuCount} cores)` : env.cpuModel);
  if (Number.isFinite(env.iterations)) envParts.push(`${env.iterations} iterations`);

  const envHtml = envParts.length ? `<div><strong>Env:</strong> ${envParts.join(" • ")}</div>` : "";

  const metricIndex = new Map();

  for (const entry of entries) {
    for (const [scenarioName, scenario] of Object.entries(entry.scenarios ?? {})) {
      for (const [metricName, metric] of Object.entries(scenario.metrics ?? {})) {
        const key = `${scenarioName}.${metricName}`;
        if (!metricIndex.has(key)) {
          metricIndex.set(key, {
            scenarioName,
            metricName,
            unit: metric.unit,
            better: metric.better,
            points: [],
          });
        }
        metricIndex.get(key).points.push({
          timestamp: entry.timestamp,
          commit: entry.commit.sha.slice(0, 7),
          url: entry.commit.url,
          y: metric.value,
          unit: metric.unit,
          n: metric.samples?.n,
          cv: metric.samples?.cv,
        });
      }
    }
  }

  const changes = [];
  for (const metric of metricIndex.values()) {
    const pts = metric.points;
    if (pts.length < 2) continue;
    const last = pts[pts.length - 1];
    const prev = pts[pts.length - 2];
    const delta = classifyDelta(prev.y, last.y, metric.better, metric.unit);
    const pct = Number.isFinite(prev.y) && prev.y !== 0 ? (last.y - prev.y) / prev.y : null;
    changes.push({
      id: `${metric.scenarioName}.${metric.metricName}`,
      pct,
      delta,
    });
  }

  const sortByMagnitudeDesc = (a, b) => Math.abs(b.pct ?? 0) - Math.abs(a.pct ?? 0);
  const topRegressions = changes
    .filter((c) => c.delta.className === "regression")
    .sort(sortByMagnitudeDesc)
    .slice(0, 5);
  const topImprovements = changes
    .filter((c) => c.delta.className === "improvement")
    .sort(sortByMagnitudeDesc)
    .slice(0, 5);

  const renderChangeList = (label, items) => {
    if (items.length === 0) return "";
    return `
      <div class="delta-list">
        <div class="delta-list-title">${label}</div>
        <ul>
          ${items
            .map(
              (c) =>
                `<li><code>${c.id}</code> <span class="delta-text ${c.delta.className}">${c.delta.text}</span></li>`,
            )
            .join("")}
        </ul>
      </div>
    `;
  };

  summary.innerHTML = `
    <div><strong>Runs:</strong> ${entries.length}</div>
    <div><strong>Schema:</strong> v${history.schemaVersion ?? "?"} • <strong>Updated:</strong> ${history.generatedAt ?? "—"}</div>
    <div><strong>Latest:</strong> ${latest.timestamp} • <a href="${latest.commit.url}" target="_blank" rel="noreferrer">${latest.commit.sha.slice(0, 7)}</a></div>
    ${envHtml}
    ${renderChangeList("Top regressions (vs prev):", topRegressions)}
    ${renderChangeList("Top improvements (vs prev):", topImprovements)}
    <div class="links">
      <a href="./history.json">history.json</a>
      <a href="./history.md">history.md</a>
      <a href="./history.schema.json">history.schema.json</a>
    </div>
  `;

  metricsContainer.innerHTML = "";

  const grouped = new Map();
  for (const metric of metricIndex.values()) {
    if (!grouped.has(metric.scenarioName)) grouped.set(metric.scenarioName, []);
    grouped.get(metric.scenarioName).push(metric);
  }

  const groupedSorted = [...grouped.entries()].sort((a, b) => a[0].localeCompare(b[0]));
  for (const [scenarioName, metrics] of groupedSorted) {
    const section = document.createElement("section");
    section.className = "scenario";
    section.innerHTML = `<h2>${prettyName(scenarioName)}</h2>`;

    metrics.sort((a, b) => a.metricName.localeCompare(b.metricName));

    for (const metric of metrics) {
      const card = document.createElement("div");
      card.className = "metric";

      const pts = metric.points;
      const last = pts[pts.length - 1];
      const prev = pts.length > 1 ? pts[pts.length - 2] : undefined;
      const delta = prev
        ? classifyDelta(prev.y, last.y, metric.better, metric.unit)
        : { className: "neutral", text: "—" };
      const samplesText = [];
      if (Number.isFinite(last.n)) samplesText.push(`n=${last.n}`);
      if (Number.isFinite(last.cv)) samplesText.push(`CV ${(last.cv * 100).toFixed(2)}%`);

      card.innerHTML = `
        <div class="metric-header">
          <div class="metric-title">${prettyName(metric.metricName)} <span class="metric-unit">(${metric.unit}, ${metric.better} is better)</span></div>
          <div class="metric-latest">
            <span class="metric-value">${formatValue(last.y)} ${metric.unit}</span>
            <span class="metric-delta ${delta.className}">${delta.text}</span>
            ${samplesText.length ? `<span class="metric-samples">${samplesText.join(" • ")}</span>` : ""}
          </div>
        </div>
      `;

      const chart = createSvgLineChart(
        pts.map((p) => ({
          timestamp: p.timestamp,
          commit: p.commit,
          url: p.url,
          y: p.y,
          unit: p.unit,
          n: p.n,
          cv: p.cv,
        })),
        {},
      );
      card.appendChild(chart);

      section.appendChild(card);
    }

    metricsContainer.appendChild(section);
  }

  const runsBody = byId("runs-table").querySelector("tbody");
  runsBody.innerHTML = "";
  for (const entry of entries.slice().reverse()) {
    const tr = document.createElement("tr");
    const sha = entry.commit.sha.slice(0, 7);
    tr.innerHTML = `<td>${entry.timestamp}</td><td><a href="${entry.commit.url}" target="_blank" rel="noreferrer">${sha}</a></td>`;
    runsBody.appendChild(tr);
  }
}

async function loadHistory() {
  const res = await fetch("./history.json", { cache: "no-store" });
  if (!res.ok) throw new Error(`Failed to fetch history.json (${res.status})`);
  return res.json();
}

async function main() {
  try {
    const history = await loadHistory();
    renderDashboard(history);
  } catch (err) {
    const status = byId("status");
    status.textContent = err instanceof Error ? err.message : String(err);
    status.classList.add("error");
  }
}

main();
