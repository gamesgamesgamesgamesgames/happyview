function handle()
  local repo = linked_repos.get(params.did)
  return { did = repo.did, status = repo.status }
end
