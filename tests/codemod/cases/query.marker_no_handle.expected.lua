local function report()
  -- codemod: input and ctx are handle's parameters in v3 -- declare function handle(input, ctx) and read them there
  return { who = caller_did, q = params.q }
end
