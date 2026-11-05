-- Empty-table encoding in both directions.
local db = require("happyview.db")
local json = require("internal.json")
local jobs = require("happyview.jobs")
function handle(input, ctx)
  local row = db.get(input.uri)
  local sent = jobs.create("t", { list = {}, arr = json.to_array({}), kept = row.record.tags, kept_map = row.record.meta, nested = { {}, { {} } }, from_input = input.empty_list, from_input_map = input.empty_map })
  return {
    fresh = {},
    marked = json.to_array({}),
    from_library_list = row.record.tags,
    from_library_map = row.record.meta,
    from_input_list = input.empty_list,
    from_input_map = input.empty_map,
    encoded = json.encode({ a = {}, b = json.to_array({}), c = row.record.tags, d = input.empty_list }),
    appended = (function() local t = row.record.tags; t[#t + 1] = "x"; return t end)(),
    cleared = (function() local t = input.items; for i = #t, 1, -1 do t[i] = nil end; return t end)(),
    sent_args = sent.echo.args,
  }
end
