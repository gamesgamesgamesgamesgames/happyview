function handle(input, ctx)
  -- codemod: db.backlinks has options this rewrite cannot map -- rebuild it as a backlinks.to(...) chain from require("happyview.backlinks")
  return db.backlinks({ uri = input.uri, sort = "createdAt" })
end
