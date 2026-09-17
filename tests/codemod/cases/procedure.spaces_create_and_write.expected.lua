local spaces = require("happyview.spaces")

function handle(input, ctx)
  local s = spaces.get((spaces.create{ type = "app.example.chat", skey = input.skey }).uri)
  s:write_record{ collection = "app.example.message", record = { text = input.text } }
  -- codemod: v3 space handles carry no fields -- read them from spaces.info(uri)
  return { uri = s.uri }
end
