local sql = require("happyview.sql")
local http = require("happyview.http")

function handle(input, ctx)
  local rows = sql.raw("SELECT 1", {})
  local resp = http.get("https://example.com")
  return { rows = rows, status = resp.status }
end
