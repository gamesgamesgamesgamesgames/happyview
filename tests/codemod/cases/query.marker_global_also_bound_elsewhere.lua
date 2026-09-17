function handle()
  local rows = db.query({ collection = "app.example.post" })
  for _, db in ipairs(rows.records) do
    log(db.uri)
  end
  return rows
end
