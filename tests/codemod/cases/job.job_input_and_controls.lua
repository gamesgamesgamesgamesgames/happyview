function handle()
  local collection = job.input.collection
  job.progress({ done = 0 })
  if job.should_stop() then
    return { id = job.id, partial = true }
  end
  job.wait(1)
  return { id = job.id, collection = collection }
end
