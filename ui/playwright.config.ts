import { defineConfig } from '@playwright/test';
export default defineConfig({ testDir: './tests', workers: 1, use: { viewport: {width:1440,height:1000}, launchOptions: {executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH} } });
