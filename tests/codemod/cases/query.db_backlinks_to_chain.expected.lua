local backlinks = require("happyview.backlinks")

function handle(input, ctx)
  return backlinks.to(input.uri):collection("app.example.like"):did(input.did):limit(20):cursor(input.cursor):run()
end
