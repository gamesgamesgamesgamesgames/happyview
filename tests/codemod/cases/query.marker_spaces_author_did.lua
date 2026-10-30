function handle()
  local page = atproto.spaces.query({ space_uri = params.uri })
  for _, rec in ipairs(page.records) do
    log(rec.authorDid)
  end
  return page
end
