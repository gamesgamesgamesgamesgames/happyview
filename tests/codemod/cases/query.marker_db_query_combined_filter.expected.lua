function handle(input, ctx)
  -- codemod: db.query has options this rewrite cannot map -- rebuild it as a db.records(...) chain from require("happyview.db")
  return db.query({
    collection = "app.example.post",
    filter = { combine = "AND", { field = "a", value = 1 }, { field = "b", value = 2 } },
  })
end
