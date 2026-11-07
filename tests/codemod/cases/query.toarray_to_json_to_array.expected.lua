local json = require("internal.json")

function handle(input, ctx)
  return { items = json.to_array({}) }
end
