local log = require("internal.logging")
local spaces = require("happyview.spaces")

function handle(input, ctx)
  local page = spaces.query({ uri = input.uri })
  for _, rec in ipairs(page.records) do
    -- codemod: v3 space records spell it author_did -- rename this read to author_did
    log.info(rec.authorDid)
  end
  -- codemod: v3 space records spell it author_did -- rename this read to author_did
  return page
end
