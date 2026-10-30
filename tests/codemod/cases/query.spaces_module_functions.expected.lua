local spaces = require("happyview.spaces")

function handle(input, ctx)
  local s = spaces.get(input.uri)
  local made = spaces.get((spaces.create({ spaceType = "app.example.chat", skey = "general" })).uri)
  local joined = spaces.get((spaces.accept_invite({ token = input.token })).uri)
  -- codemod: v3 space handles carry no fields, so returning one answers nothing -- return spaces.info(uri) or the fields you need
  return { s = s, made = made, joined = joined }
end
