function handle(input, ctx)
  return { uri = input.uri, did = ctx.caller_did }
end
