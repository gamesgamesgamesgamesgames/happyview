local spaces = require("happyview.spaces")

function handle(input, ctx)
  if input.which == "get" then
    -- codemod: v3 space handles carry no fields, so returning one answers nothing -- return spaces.info(uri) or the fields you need
    return spaces.get(input.uri)
  elseif input.which == "make" then
    -- codemod: v3 space handles carry no fields, so returning one answers nothing -- return spaces.info(uri) or the fields you need
    return spaces.get((spaces.create{ type = "app.example.chat", skey = input.skey }).uri)
  end
  -- codemod: v3 space handles carry no fields, so returning one answers nothing -- return spaces.info(uri) or the fields you need
  return { joined = spaces.get((spaces.accept_invite{ token = input.token }).uri) }
end
