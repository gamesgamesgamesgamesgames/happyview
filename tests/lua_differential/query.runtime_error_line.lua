local function helper(row)
  return row.record.title
end

function handle(input, ctx)
  local rows = {}
  return helper(rows[1])
end
