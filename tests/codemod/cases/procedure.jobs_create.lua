function handle()
  local id = jobs.create("app.example.reindex", { collection = collection }, { auth = true })
  return { job_id = id }
end
