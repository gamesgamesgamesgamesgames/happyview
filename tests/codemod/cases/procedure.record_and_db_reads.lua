function handle()
  local r = Record.load(input.uri)
  local row = db.get(input.uri)
  return { cid = r._cid, title = row.title }
end
