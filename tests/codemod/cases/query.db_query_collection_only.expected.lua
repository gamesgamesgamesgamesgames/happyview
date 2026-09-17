local db = require("happyview.db")

function handle(input, ctx)
  return db.records(ctx.collection):run()
end
