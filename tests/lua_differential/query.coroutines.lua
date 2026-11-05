local db = require("happyview.db")
function handle(input, ctx)
  -- A library call from inside a coroutine: natively this is an async
  -- function under call_async; in the guest it is a plain blocking import.
  local gen = coroutine.wrap(function()
    for _, row in ipairs(db.records("c"):run().records) do coroutine.yield(row.uri) end
  end)
  local out = {}
  for uri in gen do out[#out + 1] = uri end
  return out
end
