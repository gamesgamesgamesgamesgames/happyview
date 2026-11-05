function handle()
  local s = atproto.spaces.get(params.uri)
  return s:query{ collection = "app.example.post", limit = 10 }
end
