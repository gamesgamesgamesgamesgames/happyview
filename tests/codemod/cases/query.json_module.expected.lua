local json = require("internal.json")

function handle(input, ctx)
  local body = json.encode({ ok = true })
  return json.decode(body)
end
