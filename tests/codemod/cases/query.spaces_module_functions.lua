function handle()
  local s = atproto.spaces.get(params.uri)
  local made = atproto.spaces.create({ type = "app.example.chat", skey = "general" })
  local joined = atproto.spaces.accept_invite({ token = params.token })
  return { s = s, made = made, joined = joined }
end
