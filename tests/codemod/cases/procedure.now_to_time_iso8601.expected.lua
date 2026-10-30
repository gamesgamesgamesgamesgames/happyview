local time = require("internal.time")

function handle(input, ctx)
  return { createdAt = time.to_iso8601(time.now()) }
end
