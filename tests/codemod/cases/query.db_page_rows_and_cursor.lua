function handle()
  local page = db.query({ collection = "app.example.post", limit = params.limit, cursor = params.cursor })
  local uris = {}
  for _, row in ipairs(page.records) do
    uris[#uris + 1] = row.uri
  end
  return { uris = uris, cursor = page.cursor }
end
