local log = require("internal.logging")

function handle(input, ctx)
  local client = make_client()
  local rows = client:query{ sql = "select 1" }
  log.info("rows")
  return { rows = rows }
end
