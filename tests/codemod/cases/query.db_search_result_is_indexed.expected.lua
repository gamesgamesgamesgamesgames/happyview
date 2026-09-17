local db = require("happyview.db")

function handle(input, ctx)
  return ({ records = db.search("app.example.post", "text", input.q) }).records
end
