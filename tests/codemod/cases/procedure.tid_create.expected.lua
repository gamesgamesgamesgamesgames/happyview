local tids = require("internal.tids")

function handle(input, ctx)
  return { rkey = tids.create() }
end
