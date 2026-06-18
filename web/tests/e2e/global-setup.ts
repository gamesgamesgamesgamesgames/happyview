async function waitForService(url: string, name: string, timeoutMs = 60000) {
  const start = Date.now()
  while (Date.now() - start < timeoutMs) {
    try {
      const resp = await fetch(url)
      if (resp.ok) return
    } catch {
      // Service not ready yet
    }
    await new Promise((r) => setTimeout(r, 1000))
  }
  throw new Error(`${name} did not become healthy within ${timeoutMs}ms`)
}

async function globalSetup() {
  const baseURL = process.env.PLAYWRIGHT_BASE_URL || "http://127.0.0.1:3200"

  await waitForService(`${baseURL}/health`, "HappyView")
  await waitForService("http://localhost:2582/_health", "PLC Directory")
  await waitForService("http://localhost:3100/health", "Tranquil PDS")
}

export default globalSetup
