import { expect, test } from "@playwright/test";

test("snapshot panel: Save → Load (OPFS streaming, demo VM worker)", async ({ page }) => {
  // The snapshot demo panel lives on the `/web/` capabilities page (served under
  // the repo-root Vite harness during Playwright runs).
  await page.goto("/web/", { waitUntil: "load" });

  // The snapshot UI is part of the main capabilities page.
  const panel = page.locator(".panel", {
    has: page.getByRole("heading", { name: "Snapshots (demo VM + OPFS autosave)" }),
  });

  const saveButton = panel.getByRole("button", { name: "Save" });
  const loadButton = panel.getByRole("button", { name: "Load" });
  const deleteButton = panel.getByRole("button", { name: "Delete" });
  const status = panel.locator("pre").nth(0);

  // Wait for the panel to either become ready, or surface an error (e.g. missing
  // OPFS / missing streaming exports in older WASM builds).
  await page.waitForFunction(() => {
    const state = (window as any).__aeroDemoVmSnapshot;
    return !!state && (state.ready === true || typeof state.error === "string");
  });

  const state = await page.evaluate(() => (window as any).__aeroDemoVmSnapshot);

  if (!state?.streaming) {
    await expect(saveButton).toBeDisabled();
    await expect(status).toContainText("unavailable");
    return;
  }

  await expect(saveButton).toBeEnabled();
  await saveButton.click();
  await expect(status).toContainText("Saved snapshot");

  await loadButton.click();
  await expect(status).toContainText("Loaded snapshot");

  await deleteButton.click();
  await expect(status).toContainText("Deleted snapshot");
});
