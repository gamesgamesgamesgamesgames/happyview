import { defineConfig } from "@playwright/test"

export default defineConfig({
  testDir: "./tests/e2e",
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  workers: 1,
  reporter: process.env.CI ? [["html"], ["github"]] : [["html"]],
  use: {
    baseURL: process.env.PLAYWRIGHT_BASE_URL || "http://127.0.0.1:3200",
    trace: "on-first-retry",
  },
  projects: [
    {
      name: "no-setup",
      testMatch: "setup-gate.spec.ts",
      use: { browserName: "chromium" },
    },
    {
      name: "setup",
      testMatch: "setup-wizard.spec.ts",
      dependencies: ["no-setup"],
      use: { browserName: "chromium" },
    },
    {
      name: "post-setup",
      testMatch: [
        "service-identity-settings.spec.ts",
        "lexicon-services.spec.ts",
      ],
      dependencies: ["setup"],
      use: { browserName: "chromium" },
    },
    {
      name: "attach-account",
      testMatch: "setup-attach-account.spec.ts",
      dependencies: ["post-setup"],
      use: { browserName: "chromium" },
    },
  ],
  globalSetup: "./tests/e2e/global-setup.ts",
})
