function handle(input, ctx)
  if input.neg then
    return nil
  end
  return { src = input.src, uri = input.uri, val = input.val, cts = input.cts, exp = input.exp, raw = input }
end
