function handle(input, ctx)
  local collection = input.collection
  ctx.job.progress({ done = 0 })
  if ctx.job.should_stop() then
    return { id = ctx.job.id, partial = true }
  end
  ctx.job.wait(1)
  return { id = ctx.job.id, collection = collection }
end
