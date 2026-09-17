local jobs = require("happyview.jobs")

function handle(input, ctx)
  local id = jobs.create("app.example.reindex", { collection = ctx.collection }, { auth = true })
  return { job_id = id }
end
