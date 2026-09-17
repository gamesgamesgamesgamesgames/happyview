function handle()
  return db.backlinks({
    uri = params.uri,
    collection = "app.example.like",
    did = params.did,
    limit = 20,
    cursor = params.cursor,
  })
end
