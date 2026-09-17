function handle()
  local rec = db.get("at://did:plc:abc/app.example.post/1")
  return { record = rec, backend = db.backend() }
end
