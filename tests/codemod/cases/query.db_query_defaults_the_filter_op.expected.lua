local db = require("happyview.db")

function handle(input, ctx)
  return db.records("app.example.post"):where("status", "=", "live"):run()
end
