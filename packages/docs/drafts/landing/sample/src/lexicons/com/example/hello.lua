-- Answers com.example.hello. Save this file and the next request runs it.
function handle(input, ctx)
  return { message = "Hello, " .. (input.name or "world") .. "!" }
end
