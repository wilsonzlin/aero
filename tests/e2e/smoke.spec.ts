import { expect, test } from "@playwright/test";

test("Playwright harness is working", async ({ page }) => {
  await page.setContent("<h1>Aero</h1>");
  await expect(page.locator("h1")).toHaveText("Aero");
});

