function handle()
  return db.query({ collection = "app.example.post", sort = "createdAt", sortDirection = "asc" })
end
