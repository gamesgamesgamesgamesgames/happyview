function handle()
  if not caller_did then
    error("auth required")
  end
  return { url = env.API_URL, key = env["API_KEY"], did = caller_did }
end
