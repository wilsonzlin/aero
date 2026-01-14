import { expect, test } from "@playwright/test";

test("Boot device debug API is installed on window.aero.debug", async ({ page }) => {
  test.setTimeout(20_000);
  await page.goto("/", { waitUntil: "load" });

  await page.waitForFunction(() => {
    const dbg = (window as any).aero?.debug;
    return (
      typeof dbg?.getBootDisks === "function" &&
      typeof dbg?.getMachineCpuActiveBootDevice === "function" &&
      typeof dbg?.getMachineCpuBootConfig === "function"
    );
  });

  // No VM started yet, so boot disks, active boot device, and boot config should all be null.
  await expect(
    page.evaluate(() => {
      const dbg = (window as any).aero?.debug;
      return dbg?.getBootDisks?.() ?? "missing";
    }),
  ).resolves.toBe(null);
  await expect(
    page.evaluate(() => {
      const dbg = (window as any).aero?.debug;
      return dbg?.getMachineCpuActiveBootDevice?.() ?? "missing";
    }),
  ).resolves.toBe(null);
  await expect(
    page.evaluate(() => {
      const dbg = (window as any).aero?.debug;
      return dbg?.getMachineCpuBootConfig?.() ?? "missing";
    }),
  ).resolves.toBe(null);

  // The harness exposes a WorkerCoordinator instance for E2E tests under `window.__aeroWorkerCoordinator`.
  // This should be safe to use without starting the workers; `setBootDisks` only updates coordinator state.
  await page.waitForFunction(() => {
    const coordinator = (window as any).__aeroWorkerCoordinator;
    return !!coordinator && typeof coordinator.setBootDisks === "function";
  });

  await page.evaluate(() => {
    (window as any).__aeroWorkerCoordinator.setBootDisks({ hddId: "hdd-test", cdId: "cd-test" }, null, null);
  });
  await expect(
    page.evaluate(() => {
      return (window as any).aero?.debug?.getBootDisks?.();
    }),
  ).resolves.toEqual({ mounts: { hddId: "hdd-test", cdId: "cd-test" }, bootDevice: "cdrom" });
  await expect(
    page.evaluate(() => {
      return (window as any).aero?.debug?.getMachineCpuBootConfig?.();
    }),
  ).resolves.toBe(null);

  await page.evaluate(() => {
    (window as any).__aeroWorkerCoordinator.setBootDisks({ hddId: "hdd-test" }, null, null);
  });
  await expect(
    page.evaluate(() => {
      return (window as any).aero?.debug?.getBootDisks?.();
    }),
  ).resolves.toEqual({ mounts: { hddId: "hdd-test" }, bootDevice: "hdd" });
  await expect(
    page.evaluate(() => {
      return (window as any).aero?.debug?.getMachineCpuBootConfig?.();
    }),
  ).resolves.toBe(null);
});
