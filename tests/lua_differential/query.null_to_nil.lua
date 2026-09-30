-- JSON null in a library result is nil; JSON null in input/params is the
-- truthy null sentinel (what mlua's default to_value produces).
local db = require("happyview.db")
function handle(input, ctx)
  local row = db.get(input.uri)
  local missing = db.get("at://did:plc:alice/app.example.post/missing")
  return {
    cid_type = type(row.cid),
    indexed_at_is_nil = row.indexed_at == nil,
    reply_is_nil = row.record.reply == nil,
    missing_is_nil = missing == nil,
    missing_truthy = missing and true or false,
    input_cursor_type = type(input.cursor),
    input_cursor_truthy = input.cursor and true or false,
    params_tag_type = type(ctx.params.tag),
    delegate_type = type(ctx.delegate_did),
    echoed_back = { cursor = input.cursor, tag = ctx.params.tag },
  }
end
