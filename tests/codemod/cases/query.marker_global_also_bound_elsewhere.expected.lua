local log = require("internal.logging")

function handle(input, ctx)
  -- codemod: 'db' is bound by this script, so this use cannot be rewritten -- rename the binding, then rewrite this use by hand
  local rows = db.query({ collection = "app.example.post" })
  for _, db in ipairs(rows.records) do
    log.info(db.uri)
  end
  return rows
end
