-- codemod: this runs while the script loads, before handle receives input and ctx -- move the read into handle, or into a function handle calls
local seed = input.seed

function handle(input, ctx)
  return { seed = seed, status = input.status }
end
