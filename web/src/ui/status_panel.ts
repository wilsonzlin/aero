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

  const configPre = document.createElement("pre");
  const workerPre = document.createElement("pre");

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
