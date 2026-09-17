local db = require("happyview.db")

function handle(input, ctx)
  return { total = db.records("app.example.post"):count(), mine = db.records("app.example.post"):did(ctx.caller_did):count() }
end
