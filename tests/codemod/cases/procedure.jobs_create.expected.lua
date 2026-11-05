local jobs = require("happyview.jobs")

function handle(input, ctx)
  local id = jobs.create("app.example.reindex", { collection = "games.gamesgamesgamesgames.game" }, { auth = true })
  return { job_id = id }
end
