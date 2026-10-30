function handle(input, ctx)
  -- codemod: db.search has options this rewrite cannot map -- rewrite it as db.search(collection, field, query, limit) from require("happyview.db")
  return db.search({ collection = "app.example.post", field = "text", query = input.q, did = input.did })
end
