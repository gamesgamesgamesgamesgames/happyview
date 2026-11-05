local db = require("happyview.db")

function handle(input, ctx)
  db.search("app.example.post", "text", "x")
  return {}
end
