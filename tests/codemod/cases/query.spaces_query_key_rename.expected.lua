local spaces = require("happyview.spaces")

function handle(input, ctx)
  -- codemod: v3 space records spell it author_did -- rename this read to author_did
  return spaces.query({ uri = input.uri, collection = "app.example.post", limit = 50 })
end
