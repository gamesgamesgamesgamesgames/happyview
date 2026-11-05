local jobs = require("happyview.jobs")
function handle(input, ctx)
  local s = "héllo 😀 日本"
  local sent = jobs.create("u", { s = s, ["key_é"] = 1 })
  return { s = s, len = #s, upper = string.upper(s), sent = sent.echo.args, sub = string.sub(s, 1, 2) }
end
