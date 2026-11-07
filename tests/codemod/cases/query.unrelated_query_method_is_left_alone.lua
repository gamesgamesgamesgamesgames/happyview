function handle()
  local client = make_client()
  local rows = client:query{ sql = "select 1" }
  log("rows")
  return { rows = rows }
end
