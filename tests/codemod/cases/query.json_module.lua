function handle()
  local body = json.encode({ ok = true })
  return json.decode(body)
end
