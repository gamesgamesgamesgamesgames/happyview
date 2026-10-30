function handle()
  local s = atproto.spaces.get(params.uri)
  local page = s:query{ collection = "app.example.post", limit = 10 }
  return { records = page.records }
end
