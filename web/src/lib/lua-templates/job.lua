-- Job runner: executes as a background job. `input` is the job's
-- input table; `ctx.job` exposes the job's id, progress(data),
-- should_stop(), and wait(seconds). Return value becomes the job's result.
--
-- require("internal.*") and installed libraries provide the rest.

local log = require("internal.logging")

function handle(input, ctx)
  log.info("job started", { job_id = ctx.job.id })

  ctx.job.progress({ status = "working" })

  if ctx.job.should_stop() then
    return { partial = true }
  end

  return { done = true }
end
