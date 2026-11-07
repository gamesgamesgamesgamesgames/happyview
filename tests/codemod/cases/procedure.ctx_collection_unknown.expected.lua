local record = require("happyview.record")

function handle(input, ctx)
  -- codemod: v3 supplies no collection to a query or procedure -- write the NSID of the collection this script reads or writes
  return record.create(ctx.collection, input)
end
