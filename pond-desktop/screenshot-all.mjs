import { chromium } from '@playwright/test';
import { existsSync, mkdirSync } from 'fs';

const OUT = './screenshots';
if (!existsSync(OUT)) mkdirSync(OUT);

const SCREENS = [
  'Dashboard', 'Chat', 'Devices', 'Schedules', 'Memory',
  'Skills', 'Logs', 'Models', 'Prompts', 'Settings', 'Agent',
];

(async () => {
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });

  const errors = [];
  page.on('console', msg => { if (msg.type() === 'error') errors.push(msg.text()); });
  page.on('pageerror', err => errors.push(err.message));

  await page.goto('http://localhost:1420');
  await page.waitForTimeout(3000);

  for (const label of SCREENS) {
    errors.length = 0;
    let btn = page.locator(`button[aria-label="${label}"]`);
    if (await btn.count() === 0) {
      btn = page.locator(`button[title="${label}"]`);
    }
    if (await btn.count() === 0) {
      btn = page.locator(`button:has-text("${label}")`).first();
    }

    if (await btn.count() > 0) {
      await btn.first().click();
      await page.waitForTimeout(1200);
      await page.screenshot({ path: `${OUT}/${label.toLowerCase()}.png` });
      const errCount = errors.length;
      console.log(`${label}: ${errCount > 0 ? 'ERRORS (' + errCount + ')' : 'OK'}`);
      if (errCount > 0) errors.slice(0, 3).forEach(e => console.log(`  ${e.slice(0, 150)}`));
    } else {
      console.log(`${label}: BUTTON NOT FOUND`);
    }
  }

  await browser.close();
})();
