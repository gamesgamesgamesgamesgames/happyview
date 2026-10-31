import { defineConfig, type Project } from "@playwright/test";

// Projects that run one spec each, before `post-setup` or after it, for the
// order they need or for state the rest would disturb.
const SETUP: (Project & { testMatch: string })[] = [
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
];

const AFTER_SETUP: (Project & { testMatch: string })[] = [
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
];

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
    ...SETUP,
    {
      name: "post-setup",
      // Every spec the other projects don't claim, so a new spec runs without
      // being listed anywhere.
      testMatch: "*.spec.ts",
      testIgnore: [...SETUP, ...AFTER_SETUP].map((p) => p.testMatch),
      dependencies: ["setup"],
      use: { browserName: "chromium" },
    },
    ...AFTER_SETUP,
  ],
  globalSetup: "./tests/e2e/global-setup.ts",
});
