-- codemod: handle takes exactly (input, ctx) in v3 -- cut this parameter list down to two, then re-run the codemod
function handle(a, b, c)
  return { did = caller_did, q = params.q }
end
