-- codemod: this runs while the script loads, before handle receives input and ctx -- move the read into handle, or into a function handle calls
local BASE = env.API_URL

function handle(input, ctx)
  return { url = BASE .. "/items?q=" .. input.q }
end
