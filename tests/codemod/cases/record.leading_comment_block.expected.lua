-- Mirrors posts into a local table.
-- Runs on every indexed record.

local log = require("internal.logging")

function handle(input, ctx)
  log.info(input.uri)
  return nil
end
