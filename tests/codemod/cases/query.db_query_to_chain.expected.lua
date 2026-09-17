local db = require("happyview.db")

function handle(input, ctx)
  return db.records("app.example.post"):where("viewers", ">", 100):sort("createdAt", "asc"):limit(20):cursor(input.cursor):did(input.did):run()
end
