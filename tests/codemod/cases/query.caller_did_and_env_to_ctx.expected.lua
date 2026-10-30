function handle(input, ctx)
  if not ctx.caller_did then
    error("auth required")
  end
  return { url = ctx.env.API_URL, key = ctx.env["API_KEY"], did = ctx.caller_did }
end
