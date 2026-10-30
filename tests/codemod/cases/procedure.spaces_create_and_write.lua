function handle()
  local s = atproto.spaces.create{ type = "app.example.chat", skey = input.skey }
  s:write_record{ collection = "app.example.message", record = { text = input.text } }
  return { uri = s.uri }
end
