local spaces = require("happyview.spaces")

function handle(input, ctx)
  local s = spaces.get(input.uri)
  local page = s:records{ collection = "app.example.post", limit = 10 }
  -- codemod: v3 space records spell it author_did -- rename this read to author_did
  return { records = page.records }
end
