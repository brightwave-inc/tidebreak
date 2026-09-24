import { defineConfig, devices } from "@playwright/test";

/**
 * The end-to-end lane: the real renderer, served by a debug self-host
 * machine (decision 82), driven through Chromium. Each flow boots its own
 * machine with a scripted model and a scripted coding engine, so nothing
 * leaves the host and every answer is known in advance.
 */
export default defineConfig({
  testDir: "./tests",
  globalSetup: "./harness/global-setup.ts",
  // One flow at a time: every flow boots a machine, and a quiet host keeps the
  // lane's timing honest.
  workers: 1,
  fullyParallel: false,
  forbidOnly: Boolean(process.env.CI),
  // A flaky flow should fail the lane, not pass on a second try.
  retries: 0,
  timeout: 90_000,
  // The whole run normally takes about a minute. A run that hangs stops here;
  // Playwright then gives its teardown as long again, which still ends inside
  // the CI step's own limit, so the flows' traces are written and uploaded.
  globalTimeout: 5 * 60_000,
  expect: { timeout: 20_000 },
  outputDir: "test-results",
  reporter: process.env.CI ? [["list"], ["github"]] : [["list"]],
  use: {
    ...devices["Desktop Chrome"],
    viewport: { width: 1280, height: 860 },
    actionTimeout: 20_000,
    navigationTimeout: 30_000,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  projects: [{ name: "chromium" }],
});
