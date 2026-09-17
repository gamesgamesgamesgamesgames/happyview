local log = require("internal.logging")
local db = require("happyview.db")

-- codemod polyfill: v2 row shape over happyview.db; replace with the library API when convenient
local __codemod_flat, __codemod_flat_page, __codemod_flat_rows = (function()
  -- v2 handed a row back as the record body with `uri` written onto it; the
  -- library hands back an envelope around the body, and every read this
  -- script makes passes through here so it sees the shape it was written
  -- against.
  local function __codemod_flat(row)
    if row == nil then
      return nil
    end
    local body = row.record
    body.uri = row.uri
    return body
  end

  -- Rows are replaced inside the array they arrived in, so an empty list
  -- keeps the marking that encodes it as `[]`.
  local function __codemod_flat_rows(rows)
    for index = 1, #rows do
      rows[index] = __codemod_flat(rows[index])
    end
    return rows
  end

  -- The page goes back as it came, cursor and all.
  local function __codemod_flat_page(page)
    __codemod_flat_rows(page.records)
    return page
  end

  return __codemod_flat, __codemod_flat_page, __codemod_flat_rows
end)()

function handle(input, ctx)
  log.info("handling query", { uri = input.uri })

  if input.uri then
    local record = __codemod_flat(db.get(input.uri))
    if not record then
      error("record not found")
    end
    return { record = record }
  end

  return __codemod_flat_page(db.records(ctx.collection):limit(input.limit):cursor(input.cursor):did(input.did):run())
end
