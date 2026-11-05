-- Lazy steps accumulate in order, the immediate call dispatches once, the
-- object is reusable, and a nil argument cuts a step's list where mlua cuts it.
local db = require("happyview.db")
local sql = require("happyview.sql")
local backlinks = require("happyview.backlinks")
function handle(input, ctx)
  local q = db.records("app.example.post"):where("title", "=", input.title):sort("createdAt", "desc"):limit(input.limit)
  local count = q:count()
  local page = q:cursor(input.cursor):run()
  local first = db.records("app.example.post"):where("x", nil, 3):first()
  local no_args = db.records():run()
  local raw = sql.raw("SELECT 1 WHERE a = ?", { 1, "two", true })
  local links = backlinks.to(input.uri):collection("app.example.like"):limit(5):run()
  local by_dot = q.limit(q, 1)
  return { count = count, n = #page.records, cursor = page.cursor, first_uri = first.uri, no_args = no_args.cursor, raw = raw.echo["function"], same = by_dot == q, links = #links.records, unknown_method = type(q.nope), has_steps = #q.__steps }
end
