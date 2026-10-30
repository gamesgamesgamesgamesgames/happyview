local log = require("internal.logging")

function handle(input, ctx)
  local r = Record("app.example.post", input)
  r:save()
  log.info("record saved", { uri = r._uri })
  return { uri = r._uri, cid = r._cid }
end
