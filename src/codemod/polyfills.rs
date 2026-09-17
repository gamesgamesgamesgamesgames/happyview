//! The v2 shims a rewritten script carries in place of a global v3 dropped.
//!
//! Two globals differ in what they *mean* rather than what they are called, so
//! a rename would produce a script that runs and answers differently: `xrpc`
//! answers `{status, body}` in v2 and raises in v3, and `Record` is an object
//! held across statements in v2 and a set of unrelated functions in v3. Each
//! is shimmed once, in Lua, at the top of the script.
//!
//! A shim is meant to be retired. It is a way to keep a script correct while
//! its author rewrites it against the library, not a second supported surface.

use super::requires;

pub struct Polyfill {
    /// The global it stands in for.
    pub name: &'static str,
    /// Canonical module names its body reads. The requires block binds them
    /// and the shim closes over those locals rather than requiring its own:
    /// a Lua local is not in scope until after its own statement, so inside
    /// `local xrpc = (function() … end)()` the name `xrpc` is still the
    /// library.
    pub modules: &'static [&'static str],
    body: &'static str,
}

pub const RECORD: Polyfill = Polyfill {
    name: "Record",
    modules: &["record", "tids", "log"],
    body: include_str!("polyfills/record.lua"),
};

pub const XRPC: Polyfill = Polyfill {
    name: "xrpc",
    modules: &["xrpc", "json"],
    body: include_str!("polyfills/xrpc.lua"),
};

/// Declaration order is the order the shims are written in.
pub const ALL: [&Polyfill; 2] = [&RECORD, &XRPC];

impl Polyfill {
    /// The shim as it is spliced in: one line saying what it is and that it is
    /// temporary, then the block.
    pub fn block(&self) -> String {
        let library = requires::module_path(self.modules[0]).unwrap_or_default();
        format!(
            "-- codemod polyfill: v2 {} over {library}; replace with the library API when convenient\n{}",
            self.name, self.body
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;

    /// Libraries that answer whatever a test set up and record every call, so
    /// a shim is pinned without the plugins it stands on.
    const STUBS: &str = r#"
calls = {}
answers = {}

local function stub(module, name)
  return function(...)
    calls[#calls + 1] = { module = module, name = name, args = table.pack(...) }
    local answer = answers[module .. "." .. name]
    if type(answer) == "function" then
      return answer(...)
    end
    return answer
  end
end

local exports = {
  ["happyview.record"] = { "create", "put", "delete", "delete_local", "load", "save_local", "lexicon" },
  ["happyview.xrpc"] = { "query", "procedure" },
  ["internal.tids"] = { "create" },
  ["internal.json"] = { "encode" },
  ["internal.logging"] = { "info", "warn" },
}

omit = {}

function require(name)
  local module = {}
  for _, export in ipairs(exports[name]) do
    if not omit[name .. "." .. export] then
      module[export] = stub(name, export)
    end
  end
  return module
end
"#;

    /// Runs `script` under `polyfill`, with the requires block the rewrite
    /// would have written above it. `setup` fills in `answers`, and `omit`
    /// for an export the stubbed library should lack.
    fn run(polyfill: &Polyfill, setup: &str, script: &str) -> Lua {
        let lua = crate::lua::sandbox_for_tests();
        let requires: String = polyfill
            .modules
            .iter()
            .map(|name| {
                format!(
                    "local {name} = require(\"{}\")\n",
                    requires::module_path(name).expect("module")
                )
            })
            .collect();
        let chunk = format!(
            "{STUBS}\n{setup}\n{requires}\n{}\n{script}",
            polyfill.block()
        );
        lua.load(chunk).exec().expect("polyfill script");
        lua
    }

    fn calls(lua: &Lua) -> Vec<(String, String)> {
        let calls: mlua::Table = lua.globals().get("calls").expect("calls");
        calls
            .sequence_values::<mlua::Table>()
            .map(|call| {
                let call = call.expect("call");
                (
                    call.get::<String>("module").expect("module"),
                    call.get::<String>("name").expect("name"),
                )
            })
            .collect()
    }

    /// One argument of the n-th recorded call, as a Lua value.
    fn argument(lua: &Lua, call: usize, index: usize) -> mlua::Value {
        let calls: mlua::Table = lua.globals().get("calls").expect("calls");
        let call: mlua::Table = calls.get(call).expect("call");
        let args: mlua::Table = call.get("args").expect("args");
        args.get(index).expect("argument")
    }

    fn field(value: &mlua::Value, key: &str) -> mlua::Value {
        value
            .as_table()
            .expect("table")
            .get::<mlua::Value>(key)
            .expect("field")
    }

    fn text(value: mlua::Value) -> Option<String> {
        value.as_string().map(|string| string.to_string_lossy())
    }

    fn global(lua: &Lua, name: &str) -> mlua::Value {
        lua.globals().get(name).expect("global")
    }

    /// A lexicon answer for `app.t` declaring `properties` and a record key.
    fn lexicon(key: &str, properties: &str) -> String {
        format!(
            r#"answers["happyview.record.lexicon"] = {{ id = "app.t", defs = {{ main = {{ type = "record", key = "{key}", record = {{ properties = {properties} }} }} }} }}"#
        )
    }

    // -- Record ----------------------------------------------------------

    #[test]
    fn a_record_with_no_uri_saves_as_a_create() {
        let lua = run(
            &RECORD,
            r#"answers["happyview.record.create"] = { uri = "at://did:plc:a/app.t/1", cid = "bafy" }"#,
            r#"saved = Record("app.t", { title = "hi" }):save()"#,
        );
        assert_eq!(
            calls(&lua),
            vec![
                ("happyview.record".into(), "lexicon".into()),
                ("happyview.record".into(), "create".into()),
                ("happyview.record".into(), "save_local".into()),
            ]
        );
        assert_eq!(
            text(field(&global(&lua, "saved"), "_uri")).as_deref(),
            Some("at://did:plc:a/app.t/1")
        );
        assert_eq!(
            text(field(&global(&lua, "saved"), "_cid")).as_deref(),
            Some("bafy")
        );
    }

    #[test]
    fn a_record_that_already_has_a_uri_saves_as_a_put() {
        let lua = run(
            &RECORD,
            r#"
answers["happyview.record.load"] = { title = "hi", ["$type"] = "app.t", uri = "at://did:plc:a/app.t/1" }
answers["happyview.record.put"] = { uri = "at://did:plc:a/app.t/1", cid = "bafy2" }
"#,
            r#"
local r = Record.load("at://did:plc:a/app.t/1")
r.title = "bye"
r:save()
"#,
        );
        assert_eq!(
            calls(&lua),
            vec![
                ("happyview.record".into(), "load".into()),
                ("happyview.record".into(), "lexicon".into()),
                ("happyview.record".into(), "put".into()),
                ("happyview.record".into(), "save_local".into()),
            ]
        );
        assert_eq!(
            text(argument(&lua, 3, 1)).as_deref(),
            Some("at://did:plc:a/app.t/1")
        );
        assert_eq!(
            text(field(&argument(&lua, 3, 2), "title")).as_deref(),
            Some("bye")
        );
    }

    #[test]
    fn a_create_is_mirrored_into_the_local_index_under_the_returned_uri() {
        let lua = run(
            &RECORD,
            r#"answers["happyview.record.create"] = { uri = "at://did:plc:a/app.t/3k", cid = "bafy" }"#,
            r#"Record("app.t", { title = "hi" }):save()"#,
        );
        assert_eq!(
            calls(&lua)[2],
            ("happyview.record".into(), "save_local".into())
        );
        assert_eq!(text(argument(&lua, 3, 1)).as_deref(), Some("app.t"));
        assert_eq!(text(argument(&lua, 3, 2)).as_deref(), Some("3k"));
        assert_eq!(
            text(field(&argument(&lua, 3, 3), "title")).as_deref(),
            Some("hi")
        );
        assert_eq!(
            text(field(&argument(&lua, 3, 3), "$type")).as_deref(),
            Some("app.t")
        );
        assert_eq!(text(argument(&lua, 3, 4)).as_deref(), Some("did:plc:a"));
    }

    #[test]
    fn a_put_is_mirrored_into_the_local_index_under_its_own_uri() {
        let lua = run(
            &RECORD,
            r#"
answers["happyview.record.load"] = { title = "hi", uri = "at://did:plc:a/app.t/1" }
answers["happyview.record.put"] = { uri = "at://did:plc:a/app.t/1", cid = "bafy2" }
"#,
            r#"
local r = Record.load("at://did:plc:a/app.t/1")
r.title = "bye"
r:save()
"#,
        );
        assert_eq!(
            calls(&lua)[3],
            ("happyview.record".into(), "save_local".into())
        );
        assert_eq!(text(argument(&lua, 4, 2)).as_deref(), Some("1"));
        assert_eq!(
            text(field(&argument(&lua, 4, 3), "title")).as_deref(),
            Some("bye")
        );
        assert_eq!(text(argument(&lua, 4, 4)).as_deref(), Some("did:plc:a"));
    }

    #[test]
    fn a_mirror_that_fails_is_logged_and_the_save_still_answers() {
        let lua = run(
            &RECORD,
            r#"
answers["happyview.record.create"] = { uri = "at://did:plc:a/app.t/3k", cid = "bafy" }
answers["happyview.record.save_local"] = function() error("DB_ERROR - locked", 0) end
"#,
            r#"saved = Record("app.t", { title = "hi" }):save()"#,
        );
        assert_eq!(calls(&lua)[3], ("internal.logging".into(), "warn".into()));
        assert_eq!(
            text(field(&global(&lua, "saved"), "_cid")).as_deref(),
            Some("bafy")
        );
    }

    #[test]
    fn save_all_mirrors_each_record_and_answers_the_refs_the_library_gave() {
        let lua = run(
            &RECORD,
            r#"
local n = 0
answers["happyview.record.create"] = function()
  n = n + 1
  return { uri = "at://did:plc:a/app.t/" .. n, cid = "bafy" .. n }
end
"#,
            r#"
refs = Record.save_all({ Record("app.t", { title = "one" }), Record("app.t", { title = "two" }) })
"#,
        );
        assert_eq!(
            calls(&lua),
            vec![
                ("happyview.record".into(), "lexicon".into()),
                ("happyview.record".into(), "lexicon".into()),
                ("happyview.record".into(), "create".into()),
                ("happyview.record".into(), "save_local".into()),
                ("happyview.record".into(), "create".into()),
                ("happyview.record".into(), "save_local".into()),
            ]
        );
        let refs = global(&lua, "refs");
        let second: mlua::Value = refs.as_table().expect("table").get(2).expect("ref");
        assert_eq!(
            text(field(&second, "uri")).as_deref(),
            Some("at://did:plc:a/app.t/2")
        );
        assert_eq!(text(field(&second, "cid")).as_deref(), Some("bafy2"));
    }

    #[test]
    fn deleting_without_a_caller_raises_and_leaves_the_local_row() {
        let lua = run(
            &RECORD,
            r#"
answers["happyview.record.load"] = { title = "hi", uri = "at://did:plc:a/app.t/1" }
answers["happyview.record.delete"] = function() error("happyview.record.delete: Plugin returned error: NO_SESSION - no caller", 0) end
"#,
            r#"
local r = Record.load("at://did:plc:a/app.t/1")
ok, err = pcall(function() r:delete() end)
still = r._uri
"#,
        );
        assert_eq!(global(&lua, "ok"), mlua::Value::Boolean(false));
        assert!(
            global(&lua, "err")
                .to_string()
                .expect("message")
                .contains("no PDS auth in this script context")
        );
        assert!(
            !calls(&lua).iter().any(|call| call.1 == "delete_local"),
            "{:?}",
            calls(&lua)
        );
        assert_eq!(
            text(global(&lua, "still")).as_deref(),
            Some("at://did:plc:a/app.t/1")
        );
    }

    #[test]
    fn deleting_a_repo_the_caller_cannot_write_logs_and_drops_the_local_row() {
        let lua = run(
            &RECORD,
            r#"
answers["happyview.record.load"] = { title = "hi", uri = "at://did:plc:other/app.t/1" }
answers["happyview.record.delete"] = function() error("WRITABLE_REPO - cannot write to repo did:plc:other", 0) end
"#,
            r#"
local r = Record.load("at://did:plc:other/app.t/1")
r:delete()
gone = r._uri
"#,
        );
        assert_eq!(
            calls(&lua),
            vec![
                ("happyview.record".into(), "load".into()),
                ("happyview.record".into(), "lexicon".into()),
                ("happyview.record".into(), "delete".into()),
                ("internal.logging".into(), "warn".into()),
                ("happyview.record".into(), "delete_local".into()),
            ]
        );
        assert!(global(&lua, "gone").is_nil());
    }

    #[test]
    fn set_rkey_takes_a_number_as_v2_did() {
        let lua = run(
            &RECORD,
            r#"answers["happyview.record.create"] = { uri = "at://did:plc:a/app.t/42", cid = "bafy" }"#,
            r#"
Record("app.t", {}):set_rkey(42):save()
empty_ok = pcall(function() Record("app.t", {}):set_rkey("") end)
"#,
        );
        assert_eq!(
            text(field(&argument(&lua, 2, 3), "rkey")).as_deref(),
            Some("42")
        );
        assert_eq!(global(&lua, "empty_ok"), mlua::Value::Boolean(false));
    }

    #[test]
    fn record_new_is_the_constructor() {
        let lua = run(
            &RECORD,
            r#"answers["happyview.record.create"] = { uri = "at://did:plc:a/app.t/1", cid = "bafy" }"#,
            r#"saved = Record.new("app.t", { title = "hi" }):save()"#,
        );
        assert_eq!(
            text(field(&argument(&lua, 2, 2), "title")).as_deref(),
            Some("hi")
        );
        assert_eq!(
            text(field(&global(&lua, "saved"), "_cid")).as_deref(),
            Some("bafy")
        );
    }

    #[test]
    fn a_key_that_is_neither_string_nor_number_raises_as_v2_did() {
        let lua = run(
            &RECORD,
            r#"answers["happyview.record.create"] = { uri = "at://did:plc:a/app.t/1", cid = "bafy" }"#,
            r#"
local r = Record("app.t", { title = "hi" })
r[true] = "x"
ok, err = pcall(function() r:save() end)
"#,
        );
        assert_eq!(global(&lua, "ok"), mlua::Value::Boolean(false));
        assert!(
            global(&lua, "err")
                .to_string()
                .expect("message")
                .contains("keys must be strings")
        );
    }

    #[test]
    fn a_malformed_lexicon_property_raises_as_v2_did() {
        let lua = run(
            &RECORD,
            &lexicon("tid", r#"{ status = "not an object" }"#),
            r#"ok, err = pcall(function() return Record("app.t", {}) end)"#,
        );
        assert_eq!(global(&lua, "ok"), mlua::Value::Boolean(false));
        assert!(
            global(&lua, "err")
                .to_string()
                .expect("message")
                .contains("lexicon property 'status'")
        );
    }

    #[test]
    fn loading_strips_the_type_and_saving_puts_it_back() {
        let lua = run(
            &RECORD,
            r#"
answers["happyview.record.load"] = { title = "hi", ["$type"] = "app.t", uri = "at://did:plc:a/app.t/1" }
answers["happyview.record.put"] = { uri = "at://did:plc:a/app.t/1", cid = "bafy" }
"#,
            r#"
local r = Record.load("at://did:plc:a/app.t/1")
loaded_type = r["$type"]
loaded_uri_field = r.uri
collection = r._collection
r:save()
"#,
        );
        assert!(global(&lua, "loaded_type").is_nil());
        assert!(global(&lua, "loaded_uri_field").is_nil());
        assert_eq!(text(global(&lua, "collection")).as_deref(), Some("app.t"));
        assert_eq!(
            text(field(&argument(&lua, 3, 2), "$type")).as_deref(),
            Some("app.t")
        );
    }

    #[test]
    fn a_lexicon_bounds_the_body_a_save_sends_to_its_properties() {
        let lua = run(
            &RECORD,
            &format!(
                "{}\n{}",
                lexicon("tid", r#"{ title = { type = "string" } }"#),
                r#"answers["happyview.record.create"] = { uri = "at://did:plc:a/app.t/1", cid = "bafy" }"#
            ),
            r#"Record("app.t", { title = "hi", notify = true }):save()"#,
        );
        let body = argument(&lua, 2, 2);
        assert_eq!(text(field(&body, "title")).as_deref(), Some("hi"));
        assert!(field(&body, "notify").is_nil());
        assert_eq!(text(field(&body, "$type")).as_deref(), Some("app.t"));
    }

    #[test]
    fn without_a_lexicon_every_field_but_the_shims_own_is_sent() {
        let lua = run(
            &RECORD,
            r#"answers["happyview.record.create"] = { uri = "at://did:plc:a/app.t/1", cid = "bafy" }"#,
            r#"
local r = Record("app.t", { title = "hi", notify = true })
r._draft = true
r:save()
"#,
        );
        let body = argument(&lua, 2, 2);
        assert_eq!(text(field(&body, "title")).as_deref(), Some("hi"));
        assert_eq!(field(&body, "notify"), mlua::Value::Boolean(true));
        assert!(field(&body, "_draft").is_nil());
        assert!(field(&body, "_collection").is_nil());
    }

    #[test]
    fn a_loaded_record_is_filtered_by_its_lexicon_on_the_way_back() {
        let lua = run(
            &RECORD,
            &format!(
                "{}\n{}\n{}",
                lexicon("tid", r#"{ title = { type = "string" } }"#),
                r#"answers["happyview.record.load"] = { title = "hi", stale = 1, uri = "at://did:plc:a/app.t/1" }"#,
                r#"answers["happyview.record.put"] = { uri = "at://did:plc:a/app.t/1", cid = "bafy2" }"#
            ),
            r#"Record.load("at://did:plc:a/app.t/1"):save()"#,
        );
        let body = argument(&lua, 3, 2);
        assert_eq!(text(field(&body, "title")).as_deref(), Some("hi"));
        assert!(field(&body, "stale").is_nil());
    }

    #[test]
    fn the_lexicons_key_type_mints_a_literal_key_for_a_local_save() {
        let lua = run(
            &RECORD,
            &format!(
                "{}\n{}",
                lexicon("literal:self", "{}"),
                r#"answers["happyview.record.save_local"] = { uri = "at://did:plc:a/app.t/self" }"#
            ),
            r#"Record("app.t", {}):set_repo("did:plc:a"):save_local()"#,
        );
        assert_eq!(
            calls(&lua),
            vec![
                ("happyview.record".into(), "lexicon".into()),
                ("happyview.record".into(), "save_local".into()),
            ]
        );
        assert_eq!(text(argument(&lua, 2, 2)).as_deref(), Some("self"));
    }

    #[test]
    fn set_key_type_drives_generate_rkey() {
        let lua = run(
            &RECORD,
            r#"answers["internal.tids.create"] = "3kabcd""#,
            r#"
local r = Record("app.t", {})
literal = r:set_key_type("literal:self"):generate_rkey()
literal_rkey = r._rkey
tid = r:set_key_type("tid"):generate_rkey()
nsid_ok = pcall(function() r:set_key_type("nsid"):generate_rkey() end)
bogus_ok = pcall(function() r:set_key_type("bogus") end)
"#,
        );
        assert_eq!(text(global(&lua, "literal")).as_deref(), Some("self"));
        assert_eq!(text(global(&lua, "literal_rkey")).as_deref(), Some("self"));
        assert_eq!(text(global(&lua, "tid")).as_deref(), Some("3kabcd"));
        assert_eq!(global(&lua, "nsid_ok"), mlua::Value::Boolean(false));
        assert_eq!(global(&lua, "bogus_ok"), mlua::Value::Boolean(false));
    }

    #[test]
    fn generate_rkey_with_no_key_type_raises_rather_than_guessing() {
        let lua = run(
            &RECORD,
            "",
            r#"ok, err = pcall(function() return Record("app.t", {}):generate_rkey() end)"#,
        );
        assert_eq!(global(&lua, "ok"), mlua::Value::Boolean(false));
        assert!(
            global(&lua, "err")
                .to_string()
                .expect("message")
                .contains("set_key_type")
        );
    }

    #[test]
    fn a_library_without_lexicon_is_named_at_the_first_record() {
        let lua = run(
            &RECORD,
            r#"omit["happyview.record.lexicon"] = true"#,
            r#"
made_ok, made_err = pcall(function() return Record("app.t", {}) end)
loaded_ok, loaded_err = pcall(function() return Record.load("at://did:plc:a/app.t/1") end)
"#,
        );
        for (ok, err) in [("made_ok", "made_err"), ("loaded_ok", "loaded_err")] {
            assert_eq!(global(&lua, ok), mlua::Value::Boolean(false));
            let message = global(&lua, err).to_string().expect("message");
            assert!(
                message.contains(
                    "the Record polyfill needs the happyview-record that exports lexicon()"
                ),
                "{message}"
            );
        }
        assert!(calls(&lua).is_empty(), "{:?}", calls(&lua));
    }

    #[test]
    fn the_constructor_fills_in_the_lexicons_defaults() {
        let lua = run(
            &RECORD,
            &lexicon(
                "tid",
                r#"{ status = { type = "string", default = "draft" }, title = { type = "string" } }"#,
            ),
            r#"
local r = Record("app.t", { title = "hi" })
status = r.status
kept = Record("app.t", { status = "final" }).status
"#,
        );
        assert_eq!(text(global(&lua, "status")).as_deref(), Some("draft"));
        assert_eq!(text(global(&lua, "kept")).as_deref(), Some("final"));
    }

    #[test]
    fn a_loaded_record_carries_no_shim_bookkeeping_key() {
        let lua = run(
            &RECORD,
            r#"answers["happyview.record.load"] = { title = "hi", uri = "at://did:plc:a/app.t/1" }"#,
            r#"
local r = Record.load("at://did:plc:a/app.t/1")
keys = {}
for key in pairs(r) do
  keys[key] = true
end
cid_ok = pcall(function() return r._cid end)
"#,
        );
        let keys = global(&lua, "keys");
        assert!(field(&keys, "_loaded").is_nil());
        assert_eq!(field(&keys, "title"), mlua::Value::Boolean(true));
        assert_eq!(field(&keys, "_uri"), mlua::Value::Boolean(true));
        assert_eq!(global(&lua, "cid_ok"), mlua::Value::Boolean(false));
    }

    #[test]
    fn a_numeric_key_is_sent_under_its_string_form() {
        let lua = run(
            &RECORD,
            r#"answers["happyview.record.create"] = { uri = "at://did:plc:a/app.t/1", cid = "bafy" }"#,
            r#"
local r = Record("app.t", { title = "hi" })
r[1] = "one"
r:save()
"#,
        );
        let body = argument(&lua, 2, 2);
        assert_eq!(text(field(&body, "1")).as_deref(), Some("one"));
        assert!(
            body.as_table()
                .expect("table")
                .get::<mlua::Value>(1)
                .expect("index")
                .is_nil()
        );
    }

    #[test]
    fn the_schema_and_key_type_cannot_be_assigned() {
        let lua = run(
            &RECORD,
            "",
            r#"
schema_ok = pcall(function() Record("app.t", {})._schema = {} end)
key_ok = pcall(function() Record("app.t", {})._key_type = "tid" end)
"#,
        );
        assert_eq!(global(&lua, "schema_ok"), mlua::Value::Boolean(false));
        assert_eq!(global(&lua, "key_ok"), mlua::Value::Boolean(false));
    }

    #[test]
    fn a_missing_record_loads_as_nil() {
        let lua = run(
            &RECORD,
            "",
            r#"loaded = Record.load("at://did:plc:a/app.t/1")"#,
        );
        assert!(global(&lua, "loaded").is_nil());
    }

    #[test]
    fn a_loaded_records_cid_says_it_is_unavailable_rather_than_reading_nil() {
        let lua = run(
            &RECORD,
            r#"answers["happyview.record.load"] = { title = "hi", uri = "at://did:plc:a/app.t/1" }"#,
            r#"
local r = Record.load("at://did:plc:a/app.t/1")
ok, err = pcall(function() return r._cid end)
"#,
        );
        assert_eq!(global(&lua, "ok"), mlua::Value::Boolean(false));
        let message = global(&lua, "err");
        let message = message.to_string().expect("message");
        assert!(message.contains("_cid"), "{message}");
    }

    #[test]
    fn a_locally_saved_records_cid_reads_nil_as_it_did() {
        let lua = run(
            &RECORD,
            r#"
answers["internal.tids.create"] = "3kabcd"
answers["happyview.record.save_local"] = { uri = "at://did:plc:a/app.t/3kabcd" }
"#,
            r#"
local r = Record("app.t", { title = "hi" }):set_repo("did:plc:a"):save_local()
ok, cid = pcall(function() return r._cid end)
"#,
        );
        assert_eq!(global(&lua, "ok"), mlua::Value::Boolean(true));
        assert!(global(&lua, "cid").is_nil());
    }

    #[test]
    fn set_repo_sends_the_repo_to_the_write_that_can_refuse_it() {
        let lua = run(
            &RECORD,
            r#"answers["happyview.record.create"] = { uri = "at://did:plc:other/app.t/1", cid = "bafy" }"#,
            r#"Record("app.t", {}):set_repo("did:plc:other"):save()"#,
        );
        assert_eq!(
            text(field(&argument(&lua, 2, 3), "repo")).as_deref(),
            Some("did:plc:other")
        );
    }

    #[test]
    fn set_repo_refuses_anything_that_is_not_a_did() {
        let lua = run(
            &RECORD,
            "",
            r#"ok = pcall(function() Record("app.t", {}):set_repo("") end)"#,
        );
        assert_eq!(global(&lua, "ok"), mlua::Value::Boolean(false));
    }

    #[test]
    fn a_local_save_mints_a_tid_when_the_record_has_no_key() {
        let lua = run(
            &RECORD,
            r#"
answers["internal.tids.create"] = "3kabcd"
answers["happyview.record.save_local"] = { uri = "at://did:plc:a/app.t/3kabcd" }
"#,
            r#"Record("app.t", { title = "hi" }):set_repo("did:plc:a"):save_local()"#,
        );
        assert_eq!(
            calls(&lua),
            vec![
                ("happyview.record".into(), "lexicon".into()),
                ("internal.tids".into(), "create".into()),
                ("happyview.record".into(), "save_local".into()),
            ]
        );
        assert_eq!(text(argument(&lua, 3, 2)).as_deref(), Some("3kabcd"));
        assert_eq!(text(argument(&lua, 3, 4)).as_deref(), Some("did:plc:a"));
    }

    #[test]
    fn deleting_drops_the_local_row_even_when_the_pds_refuses() {
        let lua = run(
            &RECORD,
            r#"
answers["happyview.record.load"] = { title = "hi", uri = "at://did:plc:a/app.t/1" }
answers["happyview.record.delete"] = function() error("PDS_ERROR - PDS returned 400: nope") end
"#,
            r#"
local r = Record.load("at://did:plc:a/app.t/1")
r:delete()
gone = r._uri
"#,
        );
        assert_eq!(
            calls(&lua),
            vec![
                ("happyview.record".into(), "load".into()),
                ("happyview.record".into(), "lexicon".into()),
                ("happyview.record".into(), "delete".into()),
                ("internal.logging".into(), "warn".into()),
                ("happyview.record".into(), "delete_local".into()),
            ]
        );
        assert!(global(&lua, "gone").is_nil());
    }

    #[test]
    fn an_internal_field_cannot_be_assigned() {
        let lua = run(
            &RECORD,
            "",
            r#"ok = pcall(function() Record("app.t", {})._uri = "at://x" end)"#,
        );
        assert_eq!(global(&lua, "ok"), mlua::Value::Boolean(false));
    }

    #[test]
    fn load_all_leaves_a_missing_uri_in_its_own_place() {
        let lua = run(
            &RECORD,
            r#"
answers["happyview.record.load"] = function(uri)
  if uri == "at://did:plc:a/app.t/2" then
    return { title = "two", uri = uri }
  end
  return nil
end
"#,
            r#"
local loaded = Record.load_all({ "at://did:plc:a/app.t/1", "at://did:plc:a/app.t/2" })
first = loaded[1]
second_title = loaded[2].title
"#,
        );
        assert!(global(&lua, "first").is_nil());
        assert_eq!(text(global(&lua, "second_title")).as_deref(), Some("two"));
    }

    // -- xrpc ------------------------------------------------------------

    #[test]
    fn a_successful_call_answers_two_hundred_and_an_encoded_body() {
        let lua = run(
            &XRPC,
            r#"
answers["happyview.xrpc.query"] = { cursor = "next" }
answers["internal.json.encode"] = "{\"cursor\":\"next\"}"
"#,
            r#"response = xrpc.query("app.example.list", { limit = 5 })"#,
        );
        let response = global(&lua, "response");
        assert_eq!(field(&response, "status"), mlua::Value::Integer(200));
        assert_eq!(
            text(field(&response, "body")).as_deref(),
            Some("{\"cursor\":\"next\"}")
        );
    }

    #[test]
    fn a_failed_call_answers_the_status_the_message_carries() {
        let lua = run(
            &XRPC,
            r#"answers["happyview.xrpc.query"] = function() error("happyview.xrpc.query: Plugin returned error: XRPC_ERROR - XRPC returned 404: not found", 0) end"#,
            r#"response = xrpc.query("app.example.list")"#,
        );
        let response = global(&lua, "response");
        assert_eq!(field(&response, "status"), mlua::Value::Integer(404));
        assert!(
            field(&response, "body")
                .to_string()
                .expect("body")
                .contains("not found")
        );
    }

    #[test]
    fn a_pds_failure_answers_its_status_too() {
        let lua = run(
            &XRPC,
            r#"answers["happyview.xrpc.procedure"] = function() error("PDS_ERROR - PDS returned 403: forbidden", 0) end"#,
            r#"response = xrpc.procedure("com.atproto.repo.createRecord", {})"#,
        );
        assert_eq!(
            field(&global(&lua, "response"), "status"),
            mlua::Value::Integer(403)
        );
    }

    #[test]
    fn a_procedures_query_parameters_reach_the_library() {
        let lua = run(
            &XRPC,
            r#"
answers["happyview.xrpc.procedure"] = {}
answers["internal.json.encode"] = "{}"
"#,
            r#"xrpc.procedure("com.example.act", { x = 1 }, { p = 2 })"#,
        );
        assert_eq!(field(&argument(&lua, 1, 3), "p"), mlua::Value::Integer(2));
    }

    #[test]
    fn a_failure_naming_no_status_answers_five_hundred() {
        let lua = run(
            &XRPC,
            r#"answers["happyview.xrpc.query"] = function() error("BAD_INPUT - method is required", 0) end"#,
            r#"response = xrpc.query("")"#,
        );
        assert_eq!(
            field(&global(&lua, "response"), "status"),
            mlua::Value::Integer(500)
        );
    }

    // -- shape -----------------------------------------------------------

    #[test]
    fn every_shim_names_modules_the_requires_block_can_bind() {
        for polyfill in ALL {
            for module in polyfill.modules {
                assert!(
                    requires::module_path(module).is_some(),
                    "{}: {module}",
                    polyfill.name
                );
            }
        }
    }

    #[test]
    fn every_shim_binds_the_global_it_stands_in_for_and_says_it_is_temporary() {
        for polyfill in ALL {
            let block = polyfill.block();
            let mut lines = block.lines();
            let header = lines.next().expect("header");
            assert!(
                header.starts_with("-- codemod polyfill: v2 ")
                    && header.ends_with("; replace with the library API when convenient"),
                "{header}"
            );
            assert_eq!(
                lines.next(),
                Some(format!("local {} = (function()", polyfill.name).as_str())
            );
            assert!(full_moon::parse(&block).is_ok(), "{}", polyfill.name);
        }
    }
}
