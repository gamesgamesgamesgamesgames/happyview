local log = require("internal.logging")
local db = require("happyview.db")

function handle(input, ctx)
  log.info("listing", { collection = "games.gamesgamesgamesgames.game" })
  return db.records("games.gamesgamesgamesgames.game"):limit(input.limit or 20):run()
end
