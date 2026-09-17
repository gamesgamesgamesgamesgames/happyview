local log = require("internal.logging")
local db = require("happyview.db")

function handle(input, ctx)
  log.info("handling query", { uri = input.uri })

  if input.uri then
    local record = db.get(input.uri)
    if not record then
      error("record not found")
    end
    return { record = record }
  end

  return db.records(ctx.collection):limit(input.limit):cursor(input.cursor):did(input.did):run()
end
