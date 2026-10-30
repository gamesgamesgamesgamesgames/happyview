function handle(input, ctx)
  -- codemod: params depends on the script's trigger kind -- re-run the codemod with that kind, or rewrite it by hand
  return { q = params.q, did = ctx.caller_did }
end
