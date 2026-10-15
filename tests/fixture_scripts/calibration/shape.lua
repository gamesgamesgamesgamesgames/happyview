-- The common query handler: filter, sort, page and reshape.
local json = require("internal.json")

function handle(input, ctx)
  local kept = {}
  for _, r in ipairs(input.records) do
    if r.record.likeCount >= 5 then
      kept[#kept + 1] = r
    end
  end
  table.sort(kept, function(a, b)
    return a.record.createdAt > b.record.createdAt
  end)
  local items = {}
  for i = 1, math.min(25, #kept) do
    local r = kept[i]
    items[i] = {
      uri = r.uri,
      author = r.did,
      text = r.record.text,
      tags = r.record.tags,
      likes = r.record.likeCount,
    }
  end
  local last = kept[#items]
  return { items = json.to_array(items), cursor = last and last.record.createdAt }
end
