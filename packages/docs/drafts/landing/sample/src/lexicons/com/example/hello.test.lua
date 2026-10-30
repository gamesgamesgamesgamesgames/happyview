local test = require("internal.test")

describe("hello", function()
  it("greets by name", function()
    local res = test.query("com.example.hello", { name = "Ada" })
    assert.are.equal(200, res.status)
    assert.are.equal("Hello, Ada!", res.body.message)
  end)
end)
