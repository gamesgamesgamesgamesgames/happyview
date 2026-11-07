-- String-heavy: one formatted line per record, joined.
function handle(input, ctx)
  local lines = {}
  for i, r in ipairs(input.records) do
    local t = r.record.tags
    lines[i] = string.format('%s said "%s" (%d likes) #%s #%s #%s',
      r.did, r.record.text, r.record.likeCount, t[1], t[2], t[3])
  end
  local text = table.concat(lines, "\n")
  return { text = text, length = #text }
end
