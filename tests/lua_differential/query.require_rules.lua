local db1 = require("happyview.db")
local db2 = require("happyview.db")
local time = require("internal.time")
function handle(input, ctx)
  local ok_missing, missing = pcall(require, "happyview.nope")
  local ok_builtin, builtin = pcall(require, "internal.nope")
  local ok_type, bad_type = pcall(require, 42)
  return { cached = db1 == db2, missing = (string.match(tostring(missing), "module [^\n]+")), builtin = (string.match(tostring(builtin), "module [^\n]+")), ok_type = ok_type, time_cached = time == require("internal.time"), backend = db1.backend() }
end
