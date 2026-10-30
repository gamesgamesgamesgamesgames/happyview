local sql = require("happyview.sql")

function handle(input, ctx)
  local rows = sql.raw("SELECT uri FROM happyview_records WHERE collection = ?", { "app.example.post" })
  return { rows = rows }
end
