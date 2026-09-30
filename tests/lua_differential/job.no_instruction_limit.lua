-- Three million VM instructions: over the request limit, fine for a job.
function handle(input, ctx)
  local n = 0
  for i = 1, 1000000 do n = n + i end
  return { n = n }
end
