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
      },
      null,
      2,
    );
  }

  updateWorkers();
  globalThis.setInterval(updateWorkers, 250);
}
