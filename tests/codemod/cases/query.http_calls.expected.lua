local http = require("happyview.http")

function handle(input, ctx)
  local resp = http.get("https://example.com/data")
  http.post("https://example.com/hook", { body = resp.body })
  return { status = resp.status }
end
