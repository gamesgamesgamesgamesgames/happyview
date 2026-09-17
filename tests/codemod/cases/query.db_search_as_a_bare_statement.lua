function handle()
  db.search({ collection = "app.example.post", field = "text", query = "x" })
  return {}
end
