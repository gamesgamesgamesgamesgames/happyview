import { existsSync } from "node:fs";
import { join } from "node:path";

process.env.NODE_TLS_REJECT_UNAUTHORIZED = "0";

/**
 * The module `docker-compose.e2e.yml` mounts as the stack's `lua`
 * interpreter. Nothing in the stack fetches it, and without it HappyView
 * refuses every script save — which surfaces as several unrelated-looking
 * spec failures rather than as a missing file. It has to be in place before
 * the stack starts, so this reports rather than repairs.
 */
const INTERPRETER = join(
  __dirname,
  "../../../tests/fixtures/lua-plugin/happyview-lua.wasm",
);

async function waitForService(url: string, name: string, timeoutMs = 60000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    try {
      const resp = await fetch(url);
      if (resp.ok) return;
    } catch {
      // Service not ready yet
    }
    await new Promise((r) => setTimeout(r, 1000));
  }
  throw new Error(`${name} did not become healthy within ${timeoutMs}ms`);
}

async function globalSetup() {
  if (!existsSync(INTERPRETER)) {
    throw new Error(
      `The stack's Lua interpreter is not present, so every script save will be refused. Run:\n` +
        `  bash scripts/fetch-lua-plugin.sh\n` +
        `then restart the e2e stack. Expected: ${INTERPRETER}`,
    );
  }

  const baseURL =
    process.env.PLAYWRIGHT_BASE_URL || "https://happyview.127-0-0-1.sslip.io";

  await waitForService(`${baseURL}/health`, "HappyView");
  await waitForService("http://localhost:2582/_health", "PLC Directory");
  await waitForService("http://localhost:3100/health", "Tranquil PDS");
}

export default globalSetup;
