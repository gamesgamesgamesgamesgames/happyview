local log = require("internal.logging")
local time = require("internal.time")

function handle(input, ctx)
  log.info("hi")
  return { at = time.to_iso8601(time.now()) }
end
