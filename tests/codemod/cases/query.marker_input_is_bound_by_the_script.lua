function handle()
  local input = { defaults = true }
  return db.get(params.uri)
end
