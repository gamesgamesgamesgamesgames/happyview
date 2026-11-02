local record = require("happyview.record")

function handle(input, ctx)
  return record.create(ctx.collection, input)
end
