-- Integers and floats across the boundary, both ways.
local jobs = require("happyview.jobs")
local db = require("happyview.db")
local json = require("internal.json")
function handle(input, ctx)
  local row = db.get(input.uri)
  local sent = jobs.create("n", { int = 5, float = 5.0, half = 0.5, big = 9007199254740993, maxint = math.maxinteger, neg = -1, div = 10 / 2, idiv = 10 // 2, huge = math.huge, nan = 0 / 0, exp = 1e21 })
  return {
    count_type = math.type(row.record.count),
    score_type = math.type(row.record.score),
    limit_type = math.type(input.limit),
    ratio_type = math.type(input.record.ratio),
    sent = sent.echo.args,
    encoded = json.encode({ 1, 1.0, 1.5, 2^53, -0.0, 1e100 }),
    decoded_types = (function() local d = json.decode('[1, 1.0, 1e2, 18446744073709551615, -5, 0.1]'); local out = {}; for i, v in ipairs(d) do out[i] = math.type(v) .. ":" .. tostring(v) end; return out end)(),
    arithmetic = row.record.count + 1,
    concat = "n=" .. row.record.count .. "/" .. row.record.score,
  }
end
