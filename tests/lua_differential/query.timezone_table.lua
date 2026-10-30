-- `os.time` on a table reads it as local time, which is what PUC does. The
-- guest has no zone to read, so it reads UTC; the native runner takes the
-- server process's. The two therefore agree only when the process is on UTC,
-- and the harness expects a difference when it is not.
function handle(input, ctx)
  return { at = os.time({ year = 2026, month = 9, day = 17, hour = 12, min = 30, sec = 15 }) }
end
