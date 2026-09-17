function handle()
  return db.search({ collection = "app.example.post", field = "text", query = params.q, did = params.did })
end
