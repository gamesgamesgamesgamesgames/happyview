function handle()
  return db.backlinks({ uri = params.uri, sort = "createdAt" })
end
