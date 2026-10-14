use happyview_script::{export, query, Ctx, Result, Value};

#[query("app.test.same")]
async fn first(input: Value, _ctx: Ctx) -> Result<Value> { Ok(input) }

mod other {
    use happyview_script::{query, Ctx, Result, Value};

    #[query("app.test.same")]
    pub(crate) async fn second(input: Value, _ctx: Ctx) -> Result<Value> { Ok(input) }
}

export!(first, other::second);

fn main() {}
