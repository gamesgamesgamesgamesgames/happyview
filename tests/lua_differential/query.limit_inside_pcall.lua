-- The limit has to survive a script that catches errors.
function handle(input, ctx)
  local caught = 0
  while true do
    pcall(function() while true do end end)
    caught = caught + 1
  end
end
