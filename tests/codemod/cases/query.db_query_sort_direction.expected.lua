local db = require("happyview.db")

function handle(input, ctx)
  return db.records("app.example.post"):sort("createdAt", "asc"):run()
end
