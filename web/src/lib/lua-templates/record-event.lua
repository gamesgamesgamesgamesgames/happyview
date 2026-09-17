local log = require("internal.logging")

function handle(input, ctx)
  if input.action == "delete" then
    log.info("record deleted", { uri = input.uri })
  else
    log.info("record " .. input.action, { uri = input.uri, did = input.did })
  end

  -- A nil return skips the operation, and a delete carries no record,
  -- so `true` is what lets a delete through.
  return input.record or true
end
