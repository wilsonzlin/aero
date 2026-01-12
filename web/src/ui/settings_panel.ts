import {
  AERO_GUEST_MEMORY_PRESETS_MIB,
  AERO_LOG_LEVELS,
  parseAeroConfigOverrides,
  type AeroConfig,
  type ResolvedAeroConfig,
} from "../config/aero_config";
import type { AeroConfigManager } from "../config/manager";

export function mountSettingsPanel(container: HTMLElement, manager: AeroConfigManager): void {
  const fieldset = document.createElement("fieldset");
  const legend = document.createElement("legend");
  legend.textContent = "Settings";
  fieldset.appendChild(legend);

  const proxyHelpText =
    "Guest networking (Option C L2 tunnel). Use the gateway base URL (e.g. https://gateway.example.com or /; not /l2). " +
    "Aero will POST /session (with credentials) then connect to /l2.";

  const virtioNetModeHelpText =
    "Virtio-net PCI transport mode. Modern is the default Aero contract; transitional/legacy are for virtio-win compatibility. " +
    "Changing this requires a restart to apply.";

  const memorySelect = document.createElement("select");
  let customMemoryOption: HTMLOptionElement | null = null;
  for (const mem of AERO_GUEST_MEMORY_PRESETS_MIB) {
    const option = document.createElement("option");
    option.value = String(mem);
    option.textContent = `${mem} MiB`;
    memorySelect.appendChild(option);
  }

  const memoryHint = document.createElement("div");
  memoryHint.className = "hint";

  const workersCheckbox = document.createElement("input");
  workersCheckbox.type = "checkbox";
  const workersHint = document.createElement("div");
  workersHint.className = "hint";

  const webgpuCheckbox = document.createElement("input");
  webgpuCheckbox.type = "checkbox";
  const webgpuHint = document.createElement("div");
  webgpuHint.className = "hint";

  const proxyInput = document.createElement("input");
  proxyInput.type = "text";
  proxyInput.inputMode = "url";
  proxyInput.placeholder = "https://gateway.example.com or / (or blank)";
  proxyInput.autocomplete = "off";
  proxyInput.spellcheck = false;

  const proxyHint = document.createElement("div");
  proxyHint.className = "hint";

  const proxyError = document.createElement("div");
  proxyError.className = "hint error";

  const logSelect = document.createElement("select");
  for (const lvl of AERO_LOG_LEVELS) {
    const option = document.createElement("option");
    option.value = lvl;
    option.textContent = lvl;
    logSelect.appendChild(option);
  }
  const logHint = document.createElement("div");
  logHint.className = "hint";

  const virtioNetModeSelect = document.createElement("select");
  for (const [value, label] of [
    ["modern", "modern (default)"],
    ["transitional", "transitional (modern + legacy I/O BAR)"],
    ["legacy", "legacy-only (forces legacy driver)"],
  ] as const) {
    const option = document.createElement("option");
    option.value = value;
    option.textContent = label;
    virtioNetModeSelect.appendChild(option);
  }
  const virtioNetModeHint = document.createElement("div");
  virtioNetModeHint.className = "hint";

  const resetButton = document.createElement("button");
  resetButton.type = "button";
  resetButton.textContent = "Reset to defaults";

  fieldset.appendChild(makeRow("Guest memory", memorySelect, memoryHint));
  fieldset.appendChild(makeRow("Enable workers", workersCheckbox, workersHint));
  fieldset.appendChild(makeRow("Enable WebGPU", webgpuCheckbox, webgpuHint));
  fieldset.appendChild(makeRow("Virtio-net mode", virtioNetModeSelect, virtioNetModeHint));
  fieldset.appendChild(makeRow("Proxy URL", proxyInput, proxyHint, proxyError));
  fieldset.appendChild(makeRow("Log level", logSelect, logHint));
  fieldset.appendChild(resetButton);

  container.replaceChildren(fieldset);

  memorySelect.addEventListener("change", () => {
    manager.updateStoredConfig({ guestMemoryMiB: Number(memorySelect.value) });
  });
  workersCheckbox.addEventListener("change", () => {
    manager.updateStoredConfig({ enableWorkers: workersCheckbox.checked });
  });
  webgpuCheckbox.addEventListener("change", () => {
    manager.updateStoredConfig({ enableWebGPU: webgpuCheckbox.checked });
  });
  logSelect.addEventListener("change", () => {
    manager.updateStoredConfig({ logLevel: logSelect.value as AeroConfig["logLevel"] });
  });
  virtioNetModeSelect.addEventListener("change", () => {
    manager.updateStoredConfig({ virtioNetMode: virtioNetModeSelect.value as AeroConfig["virtioNetMode"] });
  });

  function commitProxy(): void {
    const candidate = proxyInput.value.trim();
    const parsed = parseAeroConfigOverrides({ proxyUrl: candidate });
    const issue = parsed.issues.find((i) => i.key === "proxyUrl");
    if (issue) {
      proxyError.textContent = issue.message;
      return;
    }
    proxyError.textContent = "";
    manager.updateStoredConfig({ proxyUrl: parsed.overrides.proxyUrl ?? null });
  }

  proxyInput.addEventListener("change", commitProxy);
  proxyInput.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      commitProxy();
    }
  });

  resetButton.addEventListener("click", () => {
    manager.resetToDefaults();
  });

  manager.subscribe((state) => {
    renderState(state);
  });

  function renderState(state: ResolvedAeroConfig): void {
    setLocked(memorySelect, memoryHint, state, "guestMemoryMiB");
    setLocked(logSelect, logHint, state, "logLevel");
    setLocked(virtioNetModeSelect, virtioNetModeHint, state, "virtioNetMode");
    setLocked(proxyInput, proxyHint, state, "proxyUrl");
    if (!state.lockedKeys.has("proxyUrl")) {
      proxyHint.textContent = proxyHelpText;
    }
    if (!state.lockedKeys.has("virtioNetMode")) {
      virtioNetModeHint.textContent = virtioNetModeHelpText;
    }

    const desiredMem = String(state.effective.guestMemoryMiB);
    const hasOption = Array.from(memorySelect.options).some((o) => o.value === desiredMem);
    if (!hasOption) {
      if (customMemoryOption) customMemoryOption.remove();
      customMemoryOption = document.createElement("option");
      customMemoryOption.value = desiredMem;
      customMemoryOption.textContent = `${desiredMem} MiB (custom)`;
      memorySelect.appendChild(customMemoryOption);
    } else if (customMemoryOption && customMemoryOption.value !== desiredMem) {
      customMemoryOption.remove();
      customMemoryOption = null;
    }
    memorySelect.value = desiredMem;
    logSelect.value = state.effective.logLevel;
    virtioNetModeSelect.value = state.effective.virtioNetMode ?? "modern";

    if (document.activeElement !== proxyInput) {
      proxyInput.value = state.effective.proxyUrl ?? "";
      proxyError.textContent = "";
    }

    if (!state.capabilities.supportsThreadedWorkers) {
      workersCheckbox.checked = false;
      workersCheckbox.disabled = true;
      workersHint.textContent =
        state.capabilities.threadedWorkersUnsupportedReason ?? "Threaded workers are not supported in this browser.";
    } else {
      workersCheckbox.checked = state.effective.enableWorkers;
      if (state.lockedKeys.has("enableWorkers")) {
        workersCheckbox.disabled = true;
        workersHint.textContent = "Overridden by URL query parameter.";
      } else {
        workersCheckbox.disabled = false;
        workersHint.textContent = "";
      }
    }

    if (!state.capabilities.supportsWebGPU) {
      webgpuCheckbox.checked = false;
      webgpuCheckbox.disabled = true;
      webgpuHint.textContent = state.capabilities.webgpuUnsupportedReason ?? "WebGPU is not supported in this browser.";
    } else {
      webgpuCheckbox.checked = state.effective.enableWebGPU;
      if (state.lockedKeys.has("enableWebGPU")) {
        webgpuCheckbox.disabled = true;
        webgpuHint.textContent = "Overridden by URL query parameter.";
      } else {
        webgpuCheckbox.disabled = false;
        webgpuHint.textContent = "";
      }
    }
  }
}

function makeRow(
  labelText: string,
  control: HTMLElement,
  ...hints: HTMLElement[]
): HTMLDivElement {
  const row = document.createElement("div");
  const label = document.createElement("label");
  const span = document.createElement("span");
  span.textContent = labelText;
  label.appendChild(span);
  label.appendChild(control);
  row.appendChild(label);
  for (const hint of hints) row.appendChild(hint);
  return row;
}

function setLocked(
  control: HTMLInputElement | HTMLSelectElement,
  hintEl: HTMLElement,
  state: ResolvedAeroConfig,
  key: keyof AeroConfig,
): void {
  if (state.lockedKeys.has(key)) {
    control.disabled = true;
    hintEl.textContent = "Overridden by URL query parameter.";
  } else {
    control.disabled = false;
    hintEl.textContent = "";
  }
}
