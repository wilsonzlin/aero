import { formatOneLineError as formatOneLineErrorShared } from "./_shared/text_one_line.js";

const ERROR_FMT_OPTS = Object.freeze({ includeNameFallback: true });

function formatOneLineError(err, maxBytes) {
  return formatOneLineErrorShared(err, maxBytes, ERROR_FMT_OPTS);
}

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
  entries.sort((a, b) => {
    const at =
      typeof a?.timestamp === "string" || typeof a?.timestamp === "number" || typeof a?.timestamp === "bigint"
        ? String(a.timestamp)
        : "";
    const bt =
      typeof b?.timestamp === "string" || typeof b?.timestamp === "number" || typeof b?.timestamp === "bigint"
        ? String(b.timestamp)
        : "";
    return at.localeCompare(bt);
  });
  return entries;
}

function formatValue(value) {
  if (typeof value !== "number") return "n/a";
  if (!Number.isFinite(value)) return value > 0 ? "∞" : value < 0 ? "-∞" : "n/a";
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
  if (env.gatewayMode) envParts.push(`Gateway ${env.gatewayMode}`);
  if (env.platform) {
    envParts.push(env.osRelease ? `${env.platform} ${env.osRelease}` : env.platform);
  }
  if (env.arch) envParts.push(env.arch);
  if (env.cpuModel) envParts.push(env.cpuCount ? `${env.cpuModel} (${env.cpuCount} cores)` : env.cpuModel);
  if (Number.isFinite(env.iterations)) envParts.push(`${env.iterations} iterations`);
  if (env.storageBackend) {
    envParts.push(
      env.storageApiMode ? `Storage ${env.storageBackend} (${env.storageApiMode})` : `Storage ${env.storageBackend}`,
    );
  }

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

  const buildChangeList = (label, items) => {
    if (!items.length) return null;

    const wrap = document.createElement("div");
    wrap.className = "delta-list";

    const title = document.createElement("div");
    title.className = "delta-list-title";
    title.textContent = label;
    wrap.appendChild(title);

    const ul = document.createElement("ul");
    for (const c of items) {
      const li = document.createElement("li");

      const code = document.createElement("code");
      code.textContent = c.id;
      li.appendChild(code);

      li.appendChild(document.createTextNode(" "));

      const span = document.createElement("span");
      span.className = `delta-text ${c.delta.className}`;
      span.textContent = c.delta.text;
      li.appendChild(span);

      ul.appendChild(li);
    }
    wrap.appendChild(ul);
    return wrap;
  };

  summary.replaceChildren();

  const addSummaryLine = (labelText, valueNodes) => {
    const div = document.createElement("div");
    const strong = document.createElement("strong");
    strong.textContent = labelText;
    div.appendChild(strong);
    div.appendChild(document.createTextNode(" "));
    for (const node of valueNodes) div.appendChild(node);
    summary.appendChild(div);
  };

  addSummaryLine("Runs:", [document.createTextNode(String(entries.length))]);

  const schemaVersion =
    typeof history?.schemaVersion === "string" || typeof history?.schemaVersion === "number" || typeof history?.schemaVersion === "bigint"
      ? String(history.schemaVersion)
      : "?";
  const updatedAt =
    typeof history?.generatedAt === "string" || typeof history?.generatedAt === "number" || typeof history?.generatedAt === "bigint"
      ? String(history.generatedAt)
      : "—";
  addSummaryLine("Schema:", [document.createTextNode(`v${schemaVersion} • Updated: ${updatedAt}`)]);

  const latestLine = document.createElement("span");
  const latestTimestamp =
    typeof latest?.timestamp === "string" || typeof latest?.timestamp === "number" || typeof latest?.timestamp === "bigint"
      ? String(latest.timestamp)
      : "";
  latestLine.appendChild(document.createTextNode(`${latestTimestamp} • `));

  const latestShaFull = typeof latest?.commit?.sha === "string" ? latest.commit.sha : "";
  const latestSha = latestShaFull.slice(0, 7);
  const latestUrlRaw = typeof latest?.commit?.url === "string" ? latest.commit.url : "";
  let latestHref = "";
  try {
    const u = new URL(latestUrlRaw);
    if (u.protocol === "https:" || u.protocol === "http:") latestHref = u.href;
  } catch {
    // ignore invalid URLs
  }
  if (latestHref) {
    const a = document.createElement("a");
    a.href = latestHref;
    a.target = "_blank";
    a.rel = "noreferrer";
    a.textContent = latestSha;
    latestLine.appendChild(a);
  } else {
    latestLine.appendChild(document.createTextNode(latestSha));
  }
  addSummaryLine("Latest:", [latestLine]);

  if (envParts.length) {
    addSummaryLine("Env:", [document.createTextNode(envParts.join(" • "))]);
  }

  const regressionsEl = buildChangeList("Top regressions (vs prev):", topRegressions);
  if (regressionsEl) summary.appendChild(regressionsEl);
  const improvementsEl = buildChangeList("Top improvements (vs prev):", topImprovements);
  if (improvementsEl) summary.appendChild(improvementsEl);

  const links = document.createElement("div");
  links.className = "links";
  for (const { href, text } of [
    { href: "./history.json", text: "history.json" },
    { href: "./history.md", text: "history.md" },
    { href: "./history.schema.json", text: "history.schema.json" },
  ]) {
    const a = document.createElement("a");
    a.href = href;
    a.textContent = text;
    links.appendChild(a);
  }
  summary.appendChild(links);

  metricsContainer.replaceChildren();

  const grouped = new Map();
  for (const metric of metricIndex.values()) {
    if (!grouped.has(metric.scenarioName)) grouped.set(metric.scenarioName, []);
    grouped.get(metric.scenarioName).push(metric);
  }

  const groupedSorted = [...grouped.entries()].sort((a, b) => a[0].localeCompare(b[0]));
  for (const [scenarioName, metrics] of groupedSorted) {
    const section = document.createElement("section");
    section.className = "scenario";
    const h2 = document.createElement("h2");
    h2.textContent = prettyName(scenarioName);
    section.appendChild(h2);

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

      const header = document.createElement("div");
      header.className = "metric-header";

      const title = document.createElement("div");
      title.className = "metric-title";
      title.appendChild(document.createTextNode(prettyName(metric.metricName)));

      const unitText =
        typeof metric.unit === "string" || typeof metric.unit === "number" || typeof metric.unit === "bigint"
          ? String(metric.unit)
          : "";
      const betterText = metric.better === "lower" || metric.better === "higher" ? metric.better : "unknown";
      const unitSpan = document.createElement("span");
      unitSpan.className = "metric-unit";
      unitSpan.textContent = `(${unitText}, ${betterText} is better)`;
      title.appendChild(document.createTextNode(" "));
      title.appendChild(unitSpan);

      const latestEl = document.createElement("div");
      latestEl.className = "metric-latest";

      const valueSpan = document.createElement("span");
      valueSpan.className = "metric-value";
      valueSpan.textContent = `${formatValue(last.y)}${unitText ? ` ${unitText}` : ""}`;
      latestEl.appendChild(valueSpan);

      const deltaSpan = document.createElement("span");
      deltaSpan.className = `metric-delta ${delta.className}`;
      deltaSpan.textContent = delta.text;
      latestEl.appendChild(deltaSpan);

      if (samplesText.length) {
        const samplesSpan = document.createElement("span");
        samplesSpan.className = "metric-samples";
        samplesSpan.textContent = samplesText.join(" • ");
        latestEl.appendChild(samplesSpan);
      }

      header.appendChild(title);
      header.appendChild(latestEl);
      card.appendChild(header);

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
  runsBody.replaceChildren();
  for (const entry of entries.slice().reverse()) {
    const tr = document.createElement("tr");
    const timestampText =
      typeof entry?.timestamp === "string" || typeof entry?.timestamp === "number" || typeof entry?.timestamp === "bigint"
        ? String(entry.timestamp)
        : "";

    const shaFull = typeof entry?.commit?.sha === "string" ? entry.commit.sha : "";
    const sha = shaFull.slice(0, 7);

    const tdTs = document.createElement("td");
    tdTs.textContent = timestampText;
    tr.appendChild(tdTs);

    const tdCommit = document.createElement("td");
    const urlRaw = typeof entry?.commit?.url === "string" ? entry.commit.url : "";
    let href = "";
    try {
      const u = new URL(urlRaw);
      if (u.protocol === "https:" || u.protocol === "http:") href = u.href;
    } catch {
      // ignore invalid URLs
    }
    if (href) {
      const a = document.createElement("a");
      a.href = href;
      a.target = "_blank";
      a.rel = "noreferrer";
      a.textContent = sha;
      tdCommit.appendChild(a);
    } else {
      tdCommit.textContent = sha;
    }
    tr.appendChild(tdCommit);
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
    status.textContent = formatOneLineError(err, 512);
    status.classList.add("error");
  }
}

main();
