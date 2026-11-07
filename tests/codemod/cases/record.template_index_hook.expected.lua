local log = require("internal.logging")

function handle(input, ctx)
  if input.action == "delete" then
    -- record was deleted
    log.info("deleted " .. input.uri)
  else
    -- record was created or updated
    log.info(input.action .. " " .. input.uri)
  end
end
