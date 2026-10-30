function handle()
  return db.query({ collection = "app.example.post", filter = { field = "status", value = "live" } })
end
