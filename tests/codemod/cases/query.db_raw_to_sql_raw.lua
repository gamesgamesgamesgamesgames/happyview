function handle()
  local rows = db.raw("SELECT uri FROM happyview_records WHERE collection = ?", { "app.example.post" })
  return { rows = rows }
end
