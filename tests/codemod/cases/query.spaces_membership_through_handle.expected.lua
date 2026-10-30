local spaces = require("happyview.spaces")

function handle(input, ctx)
  if spaces.get(input.uri):is_member(ctx.caller_did) then
    return {
      access = spaces.get(input.uri):access(ctx.caller_did),
      members = spaces.get(input.uri):members(),
    }
  end
  return {}
end
