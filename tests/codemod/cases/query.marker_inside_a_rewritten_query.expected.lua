local db = require("happyview.db")

function handle(input, ctx)
  -- codemod: TID.toNumber has no mechanical equivalent -- rewrite it by hand
  return db.records("app.example.post"):limit(TID.toNumber(input.n)):run()
end
