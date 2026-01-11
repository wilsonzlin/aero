import { expect, test } from "@playwright/test";

test.use({ acceptDownloads: true });

function parseSerialBytes(text: string | null): number | null {
  if (!text) return null;
  const match = text.match(/serial_bytes=([0-9,]+)/);
  if (!match) return null;
  return Number.parseInt(match[1].replaceAll(",", ""), 10);
}

test("demo VM snapshot panel saves/restores via OPFS streaming", async ({ page }, testInfo) => {
  test.setTimeout(180_000);

  // The snapshot demo panel lives on the `/web/` capabilities page (served under
  // the repo-root Vite harness during Playwright runs).
  await page.goto("/web/", { waitUntil: "load" });

  // Wait for the snapshot panel to either become ready, or surface an error (e.g. missing
  // OPFS / missing streaming exports / missing sync access handles).
  await page.waitForFunction(() => {
    const state = (window as any).__aeroDemoVmSnapshot;
    return !!state && (state.ready === true || typeof state.error === "string");
  });

  const state = await page.evaluate(() => (window as any).__aeroDemoVmSnapshot);

  const status = page.locator("#demo-vm-snapshot-status");
  const output = page.locator("#demo-vm-snapshot-output");

  const saveButton = page.locator("#demo-vm-snapshot-save");
  const loadButton = page.locator("#demo-vm-snapshot-load");
  const exportButton = page.locator("#demo-vm-snapshot-export");
  const deleteButton = page.locator("#demo-vm-snapshot-delete");
  const advanceButton = page.locator("#demo-vm-snapshot-advance");
  const importInput = page.locator("#demo-vm-snapshot-import");

  if (!state?.streaming) {
    await expect(saveButton).toBeDisabled();
    await expect(status).toContainText("unavailable");
    return;
  }

  await expect(saveButton).toBeEnabled({ timeout: 120_000 });
  await expect(loadButton).toBeEnabled();
  await expect(exportButton).toBeEnabled();
  await expect(deleteButton).toBeEnabled();
  await expect(advanceButton).toBeEnabled();

  // Clean slate (OPFS persists across test runs).
  await deleteButton.click();
  await expect(status).toContainText("Deleted snapshot", { timeout: 10_000 });

  await saveButton.click();
  await expect(status).toContainText("Saved snapshot", { timeout: 30_000 });
  const savedSerialBytes = parseSerialBytes(await status.textContent());
  expect(savedSerialBytes).not.toBeNull();

  const [download] = await Promise.all([page.waitForEvent("download"), exportButton.click()]);
  const exportedPath = testInfo.outputPath("aero-demo-vm.snap");
  await download.saveAs(exportedPath);
  await expect(status).toContainText("Exported snapshot");

  await advanceButton.click();
  await expect
    .poll(async () => parseSerialBytes(await output.textContent()), { timeout: 10_000 })
    .toBeGreaterThan(savedSerialBytes!);

  await loadButton.click();
  await expect(status).toContainText("Loaded snapshot", { timeout: 30_000 });
  const loadedSerialBytes = parseSerialBytes(await status.textContent());
  expect(loadedSerialBytes).toBe(savedSerialBytes);

  // Verify import restores state from a `.snap` file (stream-copy to OPFS + worker restore).
  await advanceButton.click();
  await expect
    .poll(async () => parseSerialBytes(await output.textContent()), { timeout: 10_000 })
    .toBeGreaterThan(savedSerialBytes!);

  await importInput.setInputFiles(exportedPath);
  await expect(status).toContainText("Imported snapshot", { timeout: 30_000 });
  const importedSerialBytes = parseSerialBytes(await status.textContent());
  expect(importedSerialBytes).toBe(savedSerialBytes);
});
