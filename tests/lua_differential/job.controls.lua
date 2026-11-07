local log = require("internal.logging")
function handle(input, ctx)
  local loops = 0
  ctx.job.progress({ stage = "start", done = 0, items = {} })
  while not ctx.job.should_stop() do
    loops = loops + 1
    ctx.job.progress({ stage = "loop", done = loops })
    ctx.job.wait(0.5)
  end
  ctx.job.progress("not a table")
  ctx.job.wait(-5)
  ctx.job.wait(99999)
  log.info("job finishing", { loops = loops })
  local keys = {}
  for k in pairs(ctx.job) do keys[#keys + 1] = k end
  table.sort(keys)
  return { id = ctx.job.id, loops = loops, keys = keys }
end
