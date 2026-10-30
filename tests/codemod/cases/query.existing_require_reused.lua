local log = require("internal.logging")

function handle(input, ctx)
  log.info("hi")
  return { at = now() }
end
