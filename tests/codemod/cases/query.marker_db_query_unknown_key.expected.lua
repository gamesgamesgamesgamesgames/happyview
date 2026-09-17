function handle(input, ctx)
  -- codemod: db.query has options this rewrite cannot map -- rebuild it as a db.records(...) chain from require("happyview.db")
  return db.query({ collection = "app.example.post", offset = 10 })
end
