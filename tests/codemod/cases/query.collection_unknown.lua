function handle()
  log("listing " .. collection)
  return db.query({ collection = collection, limit = params.limit })
end
