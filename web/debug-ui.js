const serialText = document.getElementById("serialText");
const serialCopyBtn = document.getElementById("serialCopyBtn");
const serialSaveBtn = document.getElementById("serialSaveBtn");
const serialClearBtn = document.getElementById("serialClearBtn");
const cpuStatePre = document.getElementById("cpuStatePre");
const breakpointsPre = document.getElementById("breakpointsPre");
const bpRip = document.getElementById("bpRip");
const bpAddBtn = document.getElementById("bpAddBtn");
const bpClearBtn = document.getElementById("bpClearBtn");
const tracePre = document.getElementById("tracePre");
const traceExportBtn = document.getElementById("traceExportBtn");
const traceClearBtn = document.getElementById("traceClearBtn");
const memAddr = document.getElementById("memAddr");
const memLen = document.getElementById("memLen");
const memReadBtn = document.getElementById("memReadBtn");
const memDumpPre = document.getElementById("memDumpPre");
const deviceStatePre = document.getElementById("deviceStatePre");
const deviceRefreshBtn = document.getElementById("deviceRefreshBtn");
const pauseBtn = document.getElementById("pauseBtn");
const resumeBtn = document.getElementById("resumeBtn");
const stepBtn = document.getElementById("stepBtn");

const decoder = new TextDecoder();
let traceBuffer = "";
const breakpoints = new Set();

function emitCommand(command) {
  window.dispatchEvent(new CustomEvent("aero-debug-command", { detail: command }));
}

pauseBtn.addEventListener("click", () => emitCommand({ type: "Pause" }));
resumeBtn.addEventListener("click", () => emitCommand({ type: "Resume" }));
stepBtn.addEventListener("click", () => emitCommand({ type: "Step" }));
deviceRefreshBtn.addEventListener("click", () =>
  emitCommand({ type: "RequestDeviceState" }),
);

function renderBreakpoints() {
  const list = [...breakpoints].sort((a, b) => a - b);
  if (list.length === 0) {
    breakpointsPre.textContent = "(no breakpoints)";
    return;
  }

  breakpointsPre.textContent = list
    .map((rip) => rip.toString(16).padStart(8, "0"))
    .join("\n");
}

bpAddBtn.addEventListener("click", () => {
  const parsed = Number.parseInt(bpRip.value, 16);
  if (!Number.isFinite(parsed)) return;
  breakpoints.add(parsed >>> 0);
  emitCommand({ type: "SetBreakpoint", rip: parsed >>> 0 });
  renderBreakpoints();
});

bpClearBtn.addEventListener("click", () => {
  breakpoints.clear();
  emitCommand({ type: "ClearBreakpoints" });
  renderBreakpoints();
});

serialCopyBtn.addEventListener("click", async () => {
  const text = serialText.value;
  if (!text) return;
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    serialText.select();
    document.execCommand("copy");
    serialText.setSelectionRange(serialText.value.length, serialText.value.length);
  }
});

serialSaveBtn.addEventListener("click", () => {
  const blob = new Blob([serialText.value], { type: "text/plain" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = "aero-serial.txt";
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
});

serialClearBtn.addEventListener("click", () => {
  serialText.value = "";
});

traceExportBtn.addEventListener("click", () => {
  const blob = new Blob([traceBuffer], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = "aero-trace.json";
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
});

traceClearBtn.addEventListener("click", () => {
  traceBuffer = "";
  tracePre.textContent = "(no trace yet)";
});

memReadBtn.addEventListener("click", () => {
  const addr = Number.parseInt(memAddr.value, 16) >>> 0;
  const len = Number(memLen.value) >>> 0;
  emitCommand({ type: "ReadMemory", paddr: addr, len });
});

function appendSerialBytes(bytes) {
  const chunk = decoder.decode(new Uint8Array(bytes));
  serialText.value += chunk;
  serialText.scrollTop = serialText.scrollHeight;
}

function renderHexDump(paddr, bytes) {
  const lines = [];
  const rowSize = 16;
  for (let i = 0; i < bytes.length; i += rowSize) {
    const addr = (paddr + i).toString(16).padStart(8, "0");
    const slice = bytes.slice(i, i + rowSize);
    const hex = slice
      .map((b) => b.toString(16).padStart(2, "0"))
      .join(" ");
    const ascii = slice
      .map((b) => (b >= 0x20 && b <= 0x7e ? String.fromCharCode(b) : "."))
      .join("");
    lines.push(`${addr}  ${hex.padEnd(rowSize * 3 - 1)}  |${ascii}|`);
  }
  return lines.join("\n");
}

function appendTraceEvents(events) {
  const line = events.map((e) => JSON.stringify(e)).join("\n");
  traceBuffer += line + "\n";
  tracePre.textContent = traceBuffer.trimEnd() || "(no trace yet)";
}

function onEvent(event) {
  if (!event) return;

  const kind = event.type ?? event.kind;
  switch (kind) {
    case "SerialOutput":
    case "serialOutput":
      appendSerialBytes(event.data);
      break;
    case "CpuState":
      cpuStatePre.textContent = JSON.stringify(event.state, null, 2);
      break;
    case "BreakpointHit":
      breakpoints.add(event.rip >>> 0);
      renderBreakpoints();
      break;
    case "DeviceState":
      deviceStatePre.textContent = JSON.stringify(event.state, null, 2);
      break;
    case "MemoryData":
      memDumpPre.textContent = renderHexDump(event.paddr, event.data);
      break;
    case "TraceChunk":
      appendTraceEvents(event.events);
      break;
    default:
      break;
  }
}

window.aeroDebug = {
  onEvent,
};

window.addEventListener("message", (msg) => {
  onEvent(msg.data);
});
