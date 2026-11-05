function handle(input, ctx)
  local limit = input.limit or 20
  return { limit = limit, cursor = input.cursor }
end
