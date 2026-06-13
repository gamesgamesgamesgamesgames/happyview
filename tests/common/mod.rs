#[allow(dead_code, unused_imports)]
pub mod app;
#[allow(dead_code, unused_imports)]
pub mod auth;
#[allow(dead_code, unused_imports)]
pub mod db;
#[allow(dead_code, unused_imports)]
pub mod fixtures;
#[allow(dead_code, unused_imports)]
pub mod plc;
#[allow(dead_code, unused_imports)]
pub mod tls;

#[allow(unused_macros)]
macro_rules! require_db {
    () => {
        if std::env::var("TEST_DATABASE_URL").is_err() {
            eprintln!("skipped (TEST_DATABASE_URL not set)");
            return;
        }
    };
}

#[allow(unused_imports)]
pub(crate) use require_db;
