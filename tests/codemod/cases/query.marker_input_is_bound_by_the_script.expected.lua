local db = require("happyview.db")

-- codemod: 'input' is bound by this script, so this use cannot be rewritten -- rename the binding, then rewrite this use by hand
function handle()
  local input = { defaults = true }
  -- codemod: 'input' is bound by this script, so this use cannot be rewritten -- rename the binding, then rewrite this use by hand
  return db.get(params.uri)
end
