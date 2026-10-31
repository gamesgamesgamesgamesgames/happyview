#[allow(dead_code, unused_imports)]
pub mod app;
#[allow(dead_code, unused_imports)]
pub mod auth;
#[allow(dead_code, unused_imports)]
pub mod burn;
#[allow(dead_code, unused_imports)]
pub mod db;
#[allow(dead_code, unused_imports)]
pub mod echo_interpreter;
#[allow(dead_code, unused_imports)]
pub mod fake_storage;
#[allow(unused_imports)]
pub use db::insert_oauth_session;
#[allow(dead_code, unused_imports)]
pub mod fixtures;
#[allow(dead_code, unused_imports)]
pub mod jetstream;
#[allow(dead_code, unused_imports)]
pub mod lua_plugin;
#[allow(dead_code, unused_imports)]
pub mod plc;
#[allow(dead_code, unused_imports)]
pub mod project;
#[allow(dead_code, unused_imports)]
pub mod quickjs_plugins;
#[allow(dead_code, unused_imports)]
pub mod segment_writer;
#[allow(dead_code, unused_imports)]
pub mod syncer;
#[allow(dead_code, unused_imports)]
pub mod tls;
#[allow(dead_code, unused_imports)]
pub mod watchdog;
#[allow(dead_code, unused_imports)]
pub mod workers;

#[allow(unused_macros)]
macro_rules! require_db {
    () => {
        if std::env::var("TEST_DATABASE_URL").is_err() {
            happyview::test_support::unavailable(happyview::test_support::NO_DATABASE);
            return;
        }
    };
}

#[allow(unused_imports)]
pub(crate) use require_db;
