import { expect, test } from '@playwright/test';

const DISABLE_ANIMATIONS_CSS = `
  *,
  *::before,
  *::after {
    animation-duration: 0s !important;
    animation-delay: 0s !important;
    transition-duration: 0s !important;
    transition-delay: 0s !important;
    caret-color: transparent !important;
  }
`;

test.describe('visual regression', () => {
  // Visual snapshots are only maintained for Chromium (most deterministic renderer in CI).
  test.skip(({ browserName }) => browserName !== 'chromium', 'Chromium-only visual baselines.');

  test.use({
    viewport: { width: 800, height: 600 },
    deviceScaleFactor: 1,
  });

  test.beforeEach(async ({ page }) => {
    // Apply animation/transition disabling for every navigation in this test.
    await page.addInitScript((css) => {
      const style = document.createElement('style');
      style.setAttribute('data-aero-test', 'disable-animations');
      style.textContent = css;
      document.documentElement.appendChild(style);
    }, DISABLE_ANIMATIONS_CSS);
  });

  test('synthetic aero window chrome', async ({ page }) => {
    // Visual snapshots must only cover synthetic UI we own.
    // Do NOT add Windows 7 screenshots or copyrighted imagery here.
    await page.setContent(
      `
      <!doctype html>
      <html lang="en">
        <head>
          <meta charset="utf-8" />
          <meta name="viewport" content="width=device-width, initial-scale=1" />
          <style>
            ${DISABLE_ANIMATIONS_CSS}

            :root {
              --bg: #0b1020;
              --glass: rgba(255, 255, 255, 0.10);
              --glass-2: rgba(255, 255, 255, 0.06);
              --stroke: rgba(255, 255, 255, 0.20);
              --shadow: rgba(0, 0, 0, 0.45);
              --accent: #4aa3ff;
              --good: #39d98a;
              --warn: #ffcc00;
            }

            html, body {
              height: 100%;
              margin: 0;
              background: radial-gradient(1200px 600px at 30% 20%, #15204a 0%, var(--bg) 60%, #070a12 100%);
              font-family: "DejaVu Sans", Arial, sans-serif;
              color: rgba(255, 255, 255, 0.92);
            }

            body {
              display: grid;
              place-items: center;
            }

            #window {
              width: 520px;
              height: 320px;
              border-radius: 14px;
              background: linear-gradient(180deg, var(--glass), var(--glass-2));
              border: 1px solid var(--stroke);
              box-shadow: 0 18px 50px var(--shadow);
              overflow: hidden;
              backdrop-filter: blur(12px);
              box-sizing: border-box;
            }

            .titlebar {
              height: 48px;
              display: flex;
              align-items: center;
              justify-content: space-between;
              padding: 0 12px;
              background: linear-gradient(180deg, rgba(255,255,255,0.16), rgba(255,255,255,0.06));
              border-bottom: 1px solid rgba(255, 255, 255, 0.16);
              box-sizing: border-box;
            }

            .title {
              font-size: 14px;
              font-weight: 600;
              letter-spacing: 0.2px;
            }

            .controls {
              display: flex;
              gap: 8px;
            }

            .control {
              width: 14px;
              height: 14px;
              border-radius: 999px;
              border: 1px solid rgba(255,255,255,0.35);
              background: rgba(0,0,0,0.18);
              box-shadow: inset 0 1px 2px rgba(255,255,255,0.18);
            }

            .content {
              padding: 16px;
              display: grid;
              grid-template-columns: 1fr 1fr;
              grid-template-rows: auto auto 1fr;
              gap: 12px;
              box-sizing: border-box;
            }

            .panel {
              border-radius: 12px;
              border: 1px solid rgba(255,255,255,0.16);
              background: rgba(0, 0, 0, 0.20);
              padding: 12px;
              box-sizing: border-box;
            }

            .panel h2 {
              margin: 0 0 8px 0;
              font-size: 12px;
              font-weight: 700;
              opacity: 0.92;
              text-transform: uppercase;
              letter-spacing: 0.8px;
            }

            .row {
              display: grid;
              grid-template-columns: 16px 1fr auto;
              align-items: center;
              gap: 8px;
              margin-top: 8px;
              font-size: 13px;
            }

            .dot {
              width: 10px;
              height: 10px;
              border-radius: 999px;
              background: var(--good);
              box-shadow: 0 0 0 3px rgba(57, 217, 138, 0.18);
            }

            .badge {
              font-size: 12px;
              padding: 2px 8px;
              border-radius: 999px;
              border: 1px solid rgba(255,255,255,0.18);
              background: rgba(255,255,255,0.08);
            }

            .progress {
              grid-column: span 2;
              border-radius: 12px;
              border: 1px solid rgba(255,255,255,0.16);
              background: rgba(0, 0, 0, 0.20);
              overflow: hidden;
              height: 14px;
            }

            .bar {
              height: 100%;
              width: 62%;
              background: linear-gradient(90deg, rgba(74,163,255,0.1), rgba(74,163,255,0.9));
              box-shadow: 0 0 20px rgba(74,163,255,0.6);
            }

            .cta {
              grid-column: span 2;
              display: flex;
              gap: 10px;
              justify-content: flex-end;
              align-items: center;
            }

            .button {
              border: 1px solid rgba(255,255,255,0.22);
              background: rgba(255,255,255,0.10);
              color: rgba(255,255,255,0.92);
              padding: 10px 14px;
              border-radius: 12px;
              font-size: 13px;
              font-weight: 600;
              box-shadow: 0 10px 24px rgba(0,0,0,0.28);
            }

            .button.primary {
              border-color: rgba(74,163,255,0.55);
              background: rgba(74,163,255,0.18);
            }
          </style>
        </head>
        <body>
          <div id="window" role="dialog" aria-label="Aero settings">
            <div class="titlebar">
              <div class="title">Aero • Visual Regression</div>
              <div class="controls" aria-hidden="true">
                <div class="control"></div>
                <div class="control"></div>
                <div class="control"></div>
              </div>
            </div>

            <div class="content">
              <section class="panel">
                <h2>Renderer</h2>
                <div class="row">
                  <span class="dot" aria-hidden="true"></span>
                  <span>WebGPU backend</span>
                  <span class="badge">OK</span>
                </div>
                <div class="row">
                  <span class="dot" style="background: var(--warn); box-shadow: 0 0 0 3px rgba(255, 204, 0, 0.18);" aria-hidden="true"></span>
                  <span>Shader cache</span>
                  <span class="badge">Warming</span>
                </div>
              </section>

              <section class="panel">
                <h2>Timing</h2>
                <div class="row">
                  <span class="dot" aria-hidden="true"></span>
                  <span>Frame pacing</span>
                  <span class="badge">Stable</span>
                </div>
                <div class="row">
                  <span class="dot" aria-hidden="true"></span>
                  <span>Audio sync</span>
                  <span class="badge">Locked</span>
                </div>
              </section>

              <div class="progress" aria-label="Boot progress">
                <div class="bar"></div>
              </div>

              <div class="cta">
                <button class="button" type="button">Cancel</button>
                <button class="button primary" type="button">Apply</button>
              </div>
            </div>
          </div>
        </body>
      </html>
      `,
      { waitUntil: 'load' },
    );

    // Ensure text renders with the final font metrics before we take a screenshot.
    await page.evaluate(() => document.fonts.ready);

    await expect(page.locator('#window')).toHaveScreenshot('aero-window.png');
  });
});
