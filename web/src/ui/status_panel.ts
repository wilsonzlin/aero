import type { AeroConfigManager } from "../config/manager";
import type { WorkerCoordinator } from "../runtime/coordinator";

export function mountStatusPanel(
  container: HTMLElement,
  manager: AeroConfigManager,
  coordinator: WorkerCoordinator,
): void {
  const fieldset = document.createElement("fieldset");
  const legend = document.createElement("legend");
  legend.textContent = "Status";
  fieldset.appendChild(legend);

  const controls = document.createElement("div");
  controls.className = "row";
  const restartButton = document.createElement("button");
  restartButton.textContent = "Restart VM";
  restartButton.onclick = () => coordinator.restart();
  const resetButton = document.createElement("button");
  resetButton.textContent = "Reset VM";
  resetButton.onclick = () => coordinator.reset("ui");
  const powerOffButton = document.createElement("button");
  powerOffButton.textContent = "Power off VM";
  powerOffButton.onclick = () => coordinator.powerOff();
  controls.append(restartButton, resetButton, powerOffButton);

  const configPre = document.createElement("pre");
  const workerPre = document.createElement("pre");

  fieldset.appendChild(controls);
  fieldset.appendChild(configPre);
  fieldset.appendChild(workerPre);

  container.replaceChildren(fieldset);

  manager.subscribe((state) => {
    configPre.textContent = JSON.stringify(
      {
        effectiveConfig: state.effective,
        forced: state.forced,
        lockedKeys: [...state.lockedKeys],
        issues: state.issues,
      },
      null,
      2,
    );
  });

  function updateWorkers(): void {
    workerPre.textContent = JSON.stringify(
      {
        vmState: coordinator.getVmState(),
        pendingFullRestart: coordinator.getPendingFullRestart(),
        lastFatalEvent: coordinator.getLastFatalEvent(),
        lastNonFatalEvent: coordinator.getLastNonFatalEvent(),
        configVersion: coordinator.getConfigVersion(),
        workerConfigAckVersions: coordinator.getWorkerConfigAckVersions(),
        workerStatuses: coordinator.getWorkerStatuses(),
        heartbeatCounter: coordinator.getHeartbeatCounter(),
        lastHeartbeatFromRing: coordinator.getLastHeartbeatFromRing(),
        serial: {
          bytes: coordinator.getSerialOutputBytes(),
          tail: coordinator.getSerialOutputText().slice(-512),
        },
        cpuIo: {
          irqBitmapLo: `0x${coordinator.getCpuIrqBitmapLo().toString(16)}`,
          irqBitmapHi: `0x${coordinator.getCpuIrqBitmapHi().toString(16)}`,
          a20Enabled: coordinator.getCpuA20Enabled(),
        },
        resetRequests: {
          count: coordinator.getResetRequestCount(),
          lastAtMs: coordinator.getLastResetRequestAtMs(),
        },
        bootDisks: coordinator.getBootDisks(),
        machineCpuActiveBootDevice: coordinator.getMachineCpuActiveBootDevice(),
        machineCpuBootConfig: coordinator.getMachineCpuBootConfig(),
      },
      null,
      2,
    );
  }

  updateWorkers();
  const timer = globalThis.setInterval(updateWorkers, 250);
  (timer as unknown as { unref?: () => void }).unref?.();
}
