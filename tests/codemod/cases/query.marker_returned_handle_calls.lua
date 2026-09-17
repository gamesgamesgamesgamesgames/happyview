function handle()
  if params.which == "get" then
    return atproto.spaces.get(params.uri)
  elseif params.which == "make" then
    return atproto.spaces.create{ type = "app.example.chat", skey = params.skey }
  end
  return { joined = atproto.spaces.accept_invite{ token = params.token } }
end
