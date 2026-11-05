local spaces = require("happyview.spaces")
local linked_repos = require("happyview.linked_repos")

function handle(input, ctx)
  local s = spaces.get(input.uri)
  local repo = linked_repos.get(input.did)
  -- codemod: 's' is a handle and v3 handles are never nil -- test existence with spaces.info(uri) or linked_repos.list()
  if not s then
    return { error = "no such space" }
  end
  -- codemod: 'repo' is a handle and v3 handles are never nil -- test existence with spaces.info(uri) or linked_repos.list()
  if repo == nil then
    return { error = "not linked" }
  end
  return { ok = true }
end
