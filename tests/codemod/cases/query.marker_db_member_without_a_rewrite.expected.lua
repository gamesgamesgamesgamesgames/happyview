function handle(input, ctx)
  -- codemod: db.explain has no mechanical equivalent -- rewrite it using require("happyview.db")
  return db.explain(input.q)
end
