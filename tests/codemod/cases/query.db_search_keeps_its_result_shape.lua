function handle()
  return db.search({ collection = "app.example.post", field = "text", query = params.q, limit = 25 })
end
