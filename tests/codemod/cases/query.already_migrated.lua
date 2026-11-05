local log = require("internal.logging")
local db = require("happyview.db")

function handle(input, ctx)
  log.info("listing", { collection = ctx.collection })
  return db.records(ctx.collection):limit(input.limit or 20):run()
end
