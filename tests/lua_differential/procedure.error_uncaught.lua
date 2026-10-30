local record = require("happyview.record")
function handle(input, ctx)
  local fields = { title = "x" }

  return record.create("app.example.post", { title = "boom" })
end
