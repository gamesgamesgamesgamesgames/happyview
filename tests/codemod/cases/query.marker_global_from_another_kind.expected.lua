function handle(input, ctx)
  -- codemod: rkey is not a global in a query script -- rewrite it by hand
  return { rkey = rkey }
end
