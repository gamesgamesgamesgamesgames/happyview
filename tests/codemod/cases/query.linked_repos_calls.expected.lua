local linked_repos = require("happyview.linked_repos")

function handle(input, ctx)
  local grants = linked_repos.list()
  local repo = linked_repos.get(input.did)
  repo:create_record{ collection = "app.example.post", record = { text = "hi" } }
  return { grants = grants }
end
