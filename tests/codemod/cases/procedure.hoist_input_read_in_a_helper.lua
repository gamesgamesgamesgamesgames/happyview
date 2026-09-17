local function title()
  return string.upper(input.title)
end

function handle()
  return { title = title(), debug = params.debug }
end
