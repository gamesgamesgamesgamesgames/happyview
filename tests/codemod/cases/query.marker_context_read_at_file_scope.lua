local BASE = env.API_URL

function handle()
  return { url = BASE .. "/items?q=" .. params.q }
end
