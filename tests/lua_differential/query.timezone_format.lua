-- `os.date` without a `!` renders local time, for the same reason and with
-- the same consequence: a script formatting a timestamp for display starts
-- printing UTC once the plugin is the only path.
function handle(input, ctx)
  return { shown = os.date("%Y-%m-%dT%H:%M:%S", 1789000000) }
end
