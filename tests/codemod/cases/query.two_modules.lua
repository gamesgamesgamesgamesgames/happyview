function handle()
  local rows = db.raw("SELECT 1", {})
  local resp = http.get("https://example.com")
  return { rows = rows, status = resp.status }
end
