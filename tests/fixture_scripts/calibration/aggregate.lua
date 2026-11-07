-- A record-event style computation: count tags, rank the ten most frequent.
-- Tags are ranked from the order they were first seen rather than from
-- `pairs`, whose order follows a per-run string hash seed and would make the
-- sort's work differ from run to run.
local json = require("internal.json")

function handle(input, ctx)
  local index = {}
  local ranked = {}
  for _, r in ipairs(input.records) do
    for _, tag in ipairs(r.record.tags) do
      local at = index[tag]
      if at then
        ranked[at].count = ranked[at].count + 1
      else
        ranked[#ranked + 1] = { tag = tag, count = 1 }
        index[tag] = #ranked
      end
    end
  end
  table.sort(ranked, function(a, b)
    if a.count ~= b.count then
      return a.count > b.count
    end
    return a.tag < b.tag
  end)
  return json.to_array(table.move(ranked, 1, math.min(10, #ranked), 1, {}))
end
