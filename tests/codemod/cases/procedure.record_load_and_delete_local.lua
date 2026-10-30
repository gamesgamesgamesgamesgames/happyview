function handle()
  local r = Record.load(input.uri)
  Record.delete_local(input.old_uri)
  return { record = r }
end
