function handle()
  return db.query({
    collection = "app.example.post",
    filter = { field = "viewers", op = ">", value = 100 },
    sort = "createdAt",
    sortDirection = "asc",
    limit = 20,
    cursor = params.cursor,
    did = params.did,
  })
end
