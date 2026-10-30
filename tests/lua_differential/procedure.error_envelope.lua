-- An error envelope becomes a raised error carrying the library's message.
local record = require("happyview.record")
function handle(input, ctx)
  local ok, err = pcall(record.delete, "at://did:plc:alice/app.example.post/nosession")
  local ok2, err2 = pcall(function() return record.create("app.example.post", { title = "boom" }) end)
  return {
    ok = ok, err = (string.match(tostring(err), "happyview%-record[^\n]+")),
    found_code = string.find(tostring(err), "NO_SESSION", 1, true) ~= nil,
    ok2 = ok2, err2 = (string.match(tostring(err2), "happyview%-record[^\n]+")),
  }
end
