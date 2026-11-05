function handle()
  local grants = linked_repos.list()
  local repo = linked_repos.get(params.did)
  repo:create_record{ collection = "app.example.post", record = { text = "hi" } }
  return { grants = grants }
end
