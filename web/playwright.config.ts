import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests/e2e",
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  workers: 1,
  reporter: process.env.CI ? [["html"], ["github"]] : [["html"]],
  use: {
    baseURL:
      process.env.PLAYWRIGHT_BASE_URL || "https://happyview.127-0-0-1.sslip.io",
    ignoreHTTPSErrors: true,
    trace: "retain-on-failure",
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
        "lexicon-delete.spec.ts",
        "lexicon-id-validation.spec.ts",
        "lexicon-backfill.spec.ts",
        "script-delete.spec.ts",
        "script-job.spec.ts",
        "script-unload-guard.spec.ts",
        "record-delete.spec.ts",
        "proxy-config.spec.ts",
        "spaces.spec.ts",
        "jobs.spec.ts",
        "linked-repos.spec.ts",
        "link-invite-pages.spec.ts",
        "users-add.spec.ts",
        "confidential-client.spec.ts",
        "dpop-session-revocation.spec.ts",
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
    {
      name: "linked-repos-oauth",
      testMatch: "linked-repos-oauth.spec.ts",
      dependencies: ["post-setup"],
      use: { browserName: "chromium" },
    },
    {
      name: "oauth-key-rotation",
      testMatch: "oauth-key-rotation.spec.ts",
      dependencies: ["attach-account"],
      use: { browserName: "chromium" },
    },
    {
      name: "didplc-setup",
      testMatch: "setup-didplc.spec.ts",
      dependencies: ["attach-account"],
      use: { browserName: "chromium" },
    },
    {
      name: "setup-features",
      testMatch: "setup-features.spec.ts",
      dependencies: ["didplc-setup"],
      use: { browserName: "chromium" },
    },
  ],
  globalSetup: "./tests/e2e/global-setup.ts",
});
