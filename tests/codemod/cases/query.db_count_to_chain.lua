function handle()
  return { total = db.count("app.example.post"), mine = db.count("app.example.post", caller_did) }
end
