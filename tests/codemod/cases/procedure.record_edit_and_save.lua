function handle()
  local r = Record.load(input.uri)
  r.title = input.title
  r:save()
  return { uri = r._uri }
end
