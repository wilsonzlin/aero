import { expect, test } from "@playwright/test";

test("snapshot panel: Save → Load (OPFS streaming snapshots)", async ({ page }) => {
  // The snapshot demo panel lives on the `/web/` capabilities page.
  await page.goto("http://127.0.0.1:5173/web/", { waitUntil: "load" });

  const panel = page.locator(".panel", {
    has: page.getByRole("heading", { name: "Snapshots (demo VM + OPFS autosave)" }),
  });

  const saveButton = panel.getByRole("button", { name: "Save" });
  const loadButton = panel.getByRole("button", { name: "Load" });
  const status = panel.locator("pre").nth(0);

  // Wait for the panel to either become ready, or surface an error (e.g. OPFS missing,
  // sync access handle unsupported, or streaming snapshot exports not present in the current WASM build).
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
});

