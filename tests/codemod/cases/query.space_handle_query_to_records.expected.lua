local spaces = require("happyview.spaces")

function handle(input, ctx)
  local s = spaces.get(input.uri)
  -- codemod: v3 space records spell it author_did -- rename this read to author_did
  return s:records{ collection = "app.example.post", limit = 10 }
end
