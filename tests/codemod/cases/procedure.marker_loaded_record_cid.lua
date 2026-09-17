function handle()
  local r = Record.load(input.uri)
  return { cid = r._cid }
end
