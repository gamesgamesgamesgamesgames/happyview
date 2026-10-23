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
        "backfill-accounts.spec.ts",
        "script-delete.spec.ts",
        "script-job.spec.ts",
        "script-unload-guard.spec.ts",
        "script-validate.spec.ts",
        "record-delete.spec.ts",
        "proxy-config.spec.ts",
        "spaces.spec.ts",
        "jobs.spec.ts",
        "linked-repos.spec.ts",
        "link-invite-pages.spec.ts",
        "users-add.spec.ts",
        "labelers-declared.spec.ts",
        "confidential-client.spec.ts",
        "api-client-duplicate.spec.ts",
        "dpop-session-revocation.spec.ts",
        // telemetry-settings was written but never listed here, so it
        // had never actually run.
        "telemetry-settings.spec.ts",
        "telemetry-prompt.spec.ts",
        "project-layer.spec.ts",
        "script-module-row.spec.ts",
        "plugin-catalogue.spec.ts",
        "plugin-registry-lifecycle.spec.ts",
        "lexicon-publishing.spec.ts",
        "plugin-readme-highlighting.spec.ts",
      ],
      dependencies: ["setup"],
      use: { browserName: "chromium" },
    },
    {
      name: "lexicon-publishing-oauth",
      testMatch: "lexicon-publishing-oauth.spec.ts",
      dependencies: ["post-setup"],
      use: { browserName: "chromium" },
    },
    {
      name: "attach-account",
      testMatch: "setup-attach-account.spec.ts",
      // Ordering is a stopgap for the PDS caching HappyView's client metadata
      // at its first authorization and granting no scope missing from that
      // copy. The instance client advertises the lexicon schema scope itself,
      // so every copy has the one this project needs; the scope union is
      // computed from live grants, though, so every project that authorizes
      // against the PDS runs after lexicon-publishing-oauth, keeping that
      // first fetch in one known place, after the publishing grant exists.
      dependencies: ["post-setup", "lexicon-publishing-oauth"],
      use: { browserName: "chromium" },
    },
    {
      name: "linked-repos-oauth",
      testMatch: "linked-repos-oauth.spec.ts",
      // See attach-account: the first client-metadata fetch must see the
      // publishing grant.
      dependencies: ["post-setup", "lexicon-publishing-oauth"],
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
