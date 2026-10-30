-- `input.cursor` is JSON null, so this returns the sentinel, not nil.
function handle(input, ctx) return input.cursor end
