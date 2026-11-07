local log = require("internal.logging")

function handle(input, ctx)
  log.info("starting")
  log.warn("slow")
  return { id = ctx.job.id }
end
