local linked_repos = require("happyview.linked_repos")

function handle(input, ctx)
  local repo = linked_repos.get(input.did)
  -- codemod: a linked-repos handle carries no fields -- read this one from linked_repos.list()
  return { did = repo.did, status = repo.status }
end
