//! The v2 shims a rewritten script carries where a rename would not do.
//!
//! Two globals differ in what they *mean* rather than what they are called, so
//! a rename would produce a script that runs and answers differently: `xrpc`
//! answers `{status, body}` in v2 and raises in v3, and `Record` is an object
//! held across statements in v2 and a set of unrelated functions in v3. A
//! third shim keeps the row shape `db` reads answered: v2 handed back the
//! body with `uri` on it, v3 an envelope around the body, and the reads a
//! script makes pass through it. Each is written once, in Lua, at the top of
//! the script.
//!
//! A shim is meant to be retired. It is a way to keep a script correct while
//! its author rewrites it against the library, not a second supported surface.

use std::collections::BTreeSet;

use super::requires;

pub struct Polyfill {
    /// What the header calls it: the global it stands in for, or the shape
    /// it keeps.
    pub name: &'static str,
    /// The canonical module names the header can say it stands over. It
    /// names only those the script's rewrites reached, so a script that
    /// reads through one library is not pointed at another.
    pub over: &'static [&'static str],
    /// Canonical module names its body reads. The requires block binds them
    /// and the shim closes over those locals rather than requiring its own:
    /// a Lua local is not in scope until after its own statement, so inside
    /// `local xrpc = (function() … end)()` the name `xrpc` is still the
    /// library.
    pub modules: &'static [&'static str],
    body: &'static str,
}

pub const FLATTEN: Polyfill = Polyfill {
    name: "row shape",
    over: &["db", "backlinks"],
    modules: &[],
    body: include_str!("polyfills/flatten.lua"),
};

pub const RECORD: Polyfill = Polyfill {
    name: "Record",
    over: &["record"],
    modules: &["record", "tids"],
    body: include_str!("polyfills/record.lua"),
};

pub const XRPC: Polyfill = Polyfill {
    name: "xrpc",
    over: &["xrpc"],
    modules: &["xrpc", "json"],
    body: include_str!("polyfills/xrpc.lua"),
};

/// Declaration order is the order the shims are written in.
pub const ALL: [&Polyfill; 3] = [&FLATTEN, &RECORD, &XRPC];

impl Polyfill {
    /// The shim as it is spliced in: one line saying what it is and that it is
    /// temporary, then the block. `shimmed` is the set of canonical module
    /// names whose reads went through a shim; the header names those of
    /// `over` among them, or all of `over` when none is.
    pub fn block(&self, shimmed: &BTreeSet<&str>) -> String {
        let reached: Vec<&str> = self
            .over
            .iter()
            .copied()
            .filter(|module| shimmed.contains(module))
            .collect();
        let over = if reached.is_empty() {
            self.over.to_vec()
        } else {
            reached
        };
        let libraries: Vec<&str> = over
            .iter()
            .filter_map(|module| requires::module_path(module))
            .collect();
        format!(
            "-- codemod polyfill: v2 {} over {}; replace with the library API when convenient\n{}",
            self.name,
            libraries.join(" and "),
            self.body
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
            polyfill.block(&BTreeSet::new())
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

    /// An envelope for `at://did:plc:a/app.t/1` around `body`, the shape
    /// `record.load` answers.
    fn envelope(cid: &str, body: &str) -> String {
        format!(
            r#"answers["happyview.record.load"] = {{ uri = "at://did:plc:a/app.t/1", did = "did:plc:a", collection = "app.t", rkey = "1", cid = {cid}, record = {body} }}"#
        )
    }

    // -- row shape -------------------------------------------------------

    #[test]
    fn a_flattened_row_is_the_body_with_its_own_uri_on_it() {
        let lua = run(
            &FLATTEN,
            "",
            r#"
row = __codemod_flat({ uri = "at://did:plc:a/app.t/1", did = "did:plc:a", collection = "app.t", rkey = "1", cid = "bafy", record = { title = "hi", uri = "at://stored" } })
missing = __codemod_flat(nil)
"#,
        );
        let row = global(&lua, "row");
        assert_eq!(text(field(&row, "title")).as_deref(), Some("hi"));
        assert_eq!(
            text(field(&row, "uri")).as_deref(),
            Some("at://did:plc:a/app.t/1")
        );
        assert!(field(&row, "cid").is_nil());
        assert!(field(&row, "record").is_nil());
        assert!(global(&lua, "missing").is_nil());
    }

    #[test]
    fn a_flattened_page_keeps_its_cursor_and_the_array_its_rows_came_in() {
        let lua = run(
            &FLATTEN,
            "",
            r#"
local mark = {}
local rows = setmetatable({
  { uri = "at://did:plc:a/app.t/1", record = { n = 1 } },
  { uri = "at://did:plc:a/app.t/2", record = { n = 2 } },
}, mark)
page = __codemod_flat_page({ records = rows, cursor = "next" })
same_array = page.records == rows
kept_mark = getmetatable(page.records) == mark
"#,
        );
        let page = global(&lua, "page");
        assert_eq!(text(field(&page, "cursor")).as_deref(), Some("next"));
        let second: mlua::Value = field(&page, "records")
            .as_table()
            .expect("records")
            .get(2)
            .expect("row");
        assert_eq!(field(&second, "n"), mlua::Value::Integer(2));
        assert_eq!(
            text(field(&second, "uri")).as_deref(),
            Some("at://did:plc:a/app.t/2")
        );
        assert_eq!(global(&lua, "same_array"), mlua::Value::Boolean(true));
        assert_eq!(global(&lua, "kept_mark"), mlua::Value::Boolean(true));
    }

    #[test]
    fn flattened_rows_are_the_array_they_arrived_in() {
        let lua = run(
            &FLATTEN,
            "",
            r#"
local rows = { { uri = "at://did:plc:a/app.t/1", record = { n = 1 } } }
same = __codemod_flat_rows(rows) == rows
first = rows[1]
empty = __codemod_flat_rows({})
"#,
        );
        assert_eq!(global(&lua, "same"), mlua::Value::Boolean(true));
        assert_eq!(field(&global(&lua, "first"), "n"), mlua::Value::Integer(1));
        assert!(global(&lua, "empty").as_table().expect("table").is_empty());
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
            &format!(
                "{}\n{}",
                envelope(r#""bafy""#, r#"{ title = "hi", ["$type"] = "app.t" }"#),
                r#"answers["happyview.record.put"] = { uri = "at://did:plc:a/app.t/1", cid = "bafy2" }"#
            ),
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

    /// The library writes a saved record into the index itself, so the shim
    /// touching the index too would write the same row twice.
    #[test]
    fn a_save_writes_nothing_into_the_index_itself() {
        let lua = run(
            &RECORD,
            r#"answers["happyview.record.create"] = { uri = "at://did:plc:a/app.t/3k", cid = "bafy" }"#,
            r#"saved = Record("app.t", { title = "hi" }):save()"#,
        );
        assert!(
            !calls(&lua).iter().any(|call| call.1 == "save_local"),
            "{:?}",
            calls(&lua)
        );
        assert_eq!(
            text(field(&global(&lua, "saved"), "_cid")).as_deref(),
            Some("bafy")
        );
    }

    #[test]
    fn save_all_answers_the_refs_the_library_gave() {
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
                ("happyview.record".into(), "create".into()),
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
            &format!(
                "{}\n{}",
                envelope(r#""bafy""#, r#"{ title = "hi" }"#),
                r#"answers["happyview.record.delete"] = function() error("happyview.record.delete: Plugin returned error: NO_SESSION - no caller", 0) end"#
            ),
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
    fn deleting_a_repo_the_caller_cannot_write_drops_the_local_row() {
        let lua = run(
            &RECORD,
            &format!(
                "{}\n{}",
                envelope(r#""bafy""#, r#"{ title = "hi" }"#),
                r#"answers["happyview.record.delete"] = function() error("WRITABLE_REPO - cannot write to repo did:plc:other", 0) end"#
            ),
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
            &format!(
                "{}\n{}",
                envelope(r#""bafy""#, r#"{ title = "hi", ["$type"] = "app.t" }"#),
                r#"answers["happyview.record.put"] = { uri = "at://did:plc:a/app.t/1", cid = "bafy" }"#
            ),
            r#"
local r = Record.load("at://did:plc:a/app.t/1")
loaded_type = r["$type"]
collection = r._collection
uri = r._uri
r:save()
"#,
        );
        assert!(global(&lua, "loaded_type").is_nil());
        assert_eq!(text(global(&lua, "collection")).as_deref(), Some("app.t"));
        assert_eq!(
            text(global(&lua, "uri")).as_deref(),
            Some("at://did:plc:a/app.t/1")
        );
        assert_eq!(
            text(field(&argument(&lua, 3, 2), "$type")).as_deref(),
            Some("app.t")
        );
    }

    /// The envelope keeps the body apart from the record's own URI, so a
    /// lexicon that declares a `uri` field reads back what was stored.
    #[test]
    fn a_stored_uri_field_survives_a_load() {
        let lua = run(
            &RECORD,
            &envelope(
                r#""bafy""#,
                r#"{ title = "hi", uri = "https://example.com" }"#,
            ),
            r#"
local r = Record.load("at://did:plc:a/app.t/1")
stored = r.uri
own = r._uri
"#,
        );
        assert_eq!(
            text(global(&lua, "stored")).as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            text(global(&lua, "own")).as_deref(),
            Some("at://did:plc:a/app.t/1")
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
                envelope(r#""bafy""#, r#"{ title = "hi", stale = 1 }"#),
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

    /// A record handed back as a response carries only the keys v2 gave it:
    /// its fields and the `_` bookkeeping v2 also had.
    #[test]
    fn a_loaded_record_carries_only_the_keys_v2_gave_it() {
        let lua = run(
            &RECORD,
            &envelope(r#""bafy""#, r#"{ title = "hi" }"#),
            r#"
local r = Record.load("at://did:plc:a/app.t/1")
keys = {}
for key in pairs(r) do
  keys[key] = true
end
"#,
        );
        let keys = global(&lua, "keys");
        assert_eq!(field(&keys, "title"), mlua::Value::Boolean(true));
        assert_eq!(field(&keys, "_uri"), mlua::Value::Boolean(true));
        assert_eq!(field(&keys, "_cid"), mlua::Value::Boolean(true));
        assert_eq!(field(&keys, "_collection"), mlua::Value::Boolean(true));
        assert!(field(&keys, "record").is_nil());
        assert!(field(&keys, "did").is_nil());
        assert!(field(&keys, "rkey").is_nil());
        assert!(field(&keys, "indexed_at").is_nil());
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
    fn a_loaded_records_cid_is_the_envelopes() {
        let lua = run(
            &RECORD,
            &envelope(r#""bafy""#, r#"{ title = "hi" }"#),
            r#"cid = Record.load("at://did:plc:a/app.t/1")._cid"#,
        );
        assert_eq!(text(global(&lua, "cid")).as_deref(), Some("bafy"));
    }

    /// v2 read the cid column as a string, and a `save_local` row stores an
    /// empty one; a v2 script guarding on `if r._cid then` took that branch.
    #[test]
    fn a_loaded_record_with_no_cid_reads_an_empty_string_as_v2_did() {
        let lua = run(
            &RECORD,
            &envelope("nil", r#"{ title = "hi" }"#),
            r#"
local r = Record.load("at://did:plc:a/app.t/1")
cid = r._cid
truthy = r._cid and true or false
"#,
        );
        assert_eq!(text(global(&lua, "cid")).as_deref(), Some(""));
        assert_eq!(global(&lua, "truthy"), mlua::Value::Boolean(true));
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
            &format!(
                "{}\n{}",
                envelope(r#""bafy""#, r#"{ title = "hi" }"#),
                r#"answers["happyview.record.delete"] = function() error("PDS_ERROR - PDS returned 400: nope") end"#
            ),
            r#"
local r = Record.load("at://did:plc:a/app.t/1")
r:delete()
gone = r._uri
gone_cid = r._cid
"#,
        );
        assert_eq!(
            calls(&lua),
            vec![
                ("happyview.record".into(), "load".into()),
                ("happyview.record".into(), "lexicon".into()),
                ("happyview.record".into(), "delete".into()),
                ("happyview.record".into(), "delete_local".into()),
            ]
        );
        assert!(global(&lua, "gone").is_nil());
        assert!(global(&lua, "gone_cid").is_nil());
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
    return { uri = uri, did = "did:plc:a", collection = "app.t", rkey = "2", cid = "bafy", record = { title = "two" } }
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
            assert!(!polyfill.over.is_empty(), "{}", polyfill.name);
            for module in polyfill.over.iter().chain(polyfill.modules) {
                assert!(
                    requires::module_path(module).is_some(),
                    "{}: {module}",
                    polyfill.name
                );
            }
        }
    }

    #[test]
    fn the_row_shape_header_names_the_libraries_the_script_reads_through() {
        let header = |shimmed: &[&str]| {
            FLATTEN
                .block(&shimmed.iter().copied().collect())
                .lines()
                .next()
                .expect("header")
                .to_string()
        };
        assert_eq!(
            header(&["backlinks"]),
            "-- codemod polyfill: v2 row shape over happyview.backlinks; replace with the library API when convenient"
        );
        assert_eq!(
            header(&["db", "record"]),
            "-- codemod polyfill: v2 row shape over happyview.db; replace with the library API when convenient"
        );
        assert_eq!(
            header(&["db", "backlinks"]),
            "-- codemod polyfill: v2 row shape over happyview.db and happyview.backlinks; replace with the library API when convenient"
        );
        assert_eq!(header(&[]), header(&["db", "backlinks"]));
    }

    #[test]
    fn every_shim_is_one_local_block_under_a_header_that_says_it_is_temporary() {
        for polyfill in ALL {
            let block = polyfill.block(&BTreeSet::new());
            let mut lines = block.lines();
            let header = lines.next().expect("header");
            assert!(
                header.starts_with("-- codemod polyfill: v2 ")
                    && header.ends_with("; replace with the library API when convenient"),
                "{header}"
            );
            let opening = lines.next().expect("opening line");
            assert!(
                opening.starts_with("local ") && opening.ends_with(" = (function()"),
                "{}: {opening}",
                polyfill.name
            );
            assert!(full_moon::parse(&block).is_ok(), "{}", polyfill.name);
        }
    }

    #[test]
    fn a_shim_that_stands_in_for_a_global_binds_that_name() {
        for polyfill in [&RECORD, &XRPC] {
            assert_eq!(
                polyfill.block(&BTreeSet::new()).lines().nth(1),
                Some(format!("local {} = (function()", polyfill.name).as_str())
            );
        }
        assert_eq!(
            FLATTEN.block(&BTreeSet::new()).lines().nth(1),
            Some("local __codemod_flat, __codemod_flat_page, __codemod_flat_rows = (function()")
        );
    }
}
