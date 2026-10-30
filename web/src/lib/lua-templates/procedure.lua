local log = require("internal.logging")
local record = require("happyview.record")

function handle(input, ctx)
  local ref = record.create("COLLECTION", input)
  log.info("record saved", { uri = ref.uri })
  return { uri = ref.uri, cid = ref.cid }
end
