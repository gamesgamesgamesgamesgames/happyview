local input, ctx

local function title()
  return string.upper(input.title)
end

function handle(handle_input, handle_ctx)
  input, ctx = handle_input, handle_ctx
  return { title = title(), debug = ctx.params.debug }
end
