local log = require("internal.logging")

function handle(input, ctx)
  log.info("handling query", { uri = input.uri })

  if input.uri then
    local record = db.get(input.uri)
    if not record then
      error("record not found")
    end
    return { record = record }
  end

  return db.query({
    collection = ctx.collection,
    did = input.did,
    limit = input.limit,
    cursor = input.cursor,
  })
end
