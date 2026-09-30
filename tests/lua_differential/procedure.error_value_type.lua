-- What a caught library error *is*. Natively it is mlua's error userdata;
-- a guest has no such type to hand back.
local record = require("happyview.record")
function handle(input, ctx)
  local ok, err = pcall(record.delete, "at://did:plc:alice/app.example.post/nosession")
  local ok_concat = pcall(function() return "failed: " .. err end)
  return { type = type(err), concat_works = ok_concat }
end
