local log = require("internal.logging")
local db = require("happyview.db")

function handle(input, ctx)
  log.info("handling query", { uri = input.uri })

  if input.uri then
    local entry = db.get(input.uri)
    if not entry then
      return { error = "NotFound", message = "no record at " .. input.uri }
    end
    return { uri = entry.uri, cid = entry.cid, value = entry.record }
  end

  -- Query params arrive as strings unless the lexicon types them, and a
  -- chain step given nil is skipped, so absent filters read as no filter.
  return db.records(ctx.collection)
    :did(input.did)
    :limit(tonumber(input.limit))
    :cursor(input.cursor)
    :run()
end
