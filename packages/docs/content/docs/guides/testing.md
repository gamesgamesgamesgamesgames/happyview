---
title: "Testing"
---

`happyview test <root>` builds the project at `<root>` with its tests included and runs every test file in it. A test file is any file under `src/` named `*.test.<ext>`, where `<ext>` is an extension an installed interpreter claims, such as `src/lib/format.test.lua`. `hpy happyview test` runs the same command with the project's pinned HappyView, after building the project's compiled modules.

```bash
happyview test .                  # every test file
happyview test . format           # only files whose path contains "format"
happyview test . --watch          # run, then rerun on every save
happyview test . --plugin ./plugins/lua   # load this interpreter instead of the project's
```

Interpreters come from `--plugin <dir>` (repeatable), else `$PLUGIN_DIR`, else the project's `.happyview/plugins`, as with [`happyview check`](../getting-started/configuration.md#checking-a-project).

## Output and exit codes

Each file gets a line, then each failing test is listed with the place of the assertion that failed:

```
 PASS  src/lib/format.test.lua (3 tests) 12ms
 FAIL  src/lexicons/com/example/getThing.test.lua (2 tests, 1 failed) 48ms

 FAIL  src/lexicons/com/example/getThing.test.lua > getThing > answers 404 for a missing id
       src/lexicons/com/example/getThing.test.lua:14: expected 404, got 200

Test files  1 failed, 1 passed (2)
     Tests  1 failed, 4 passed (5)
  Duration  0.31s
```

A file can fail as a whole: it would not load, it raised an error outside a test, it reported no tests at all, or it ran past its time limit. That error is printed under the file's line, placed in the file it happened in, which may be a module the test file required.

| Exit code | Meaning |
| --- | --- |
| `0` | Every test file ran and passed |
| `1` | A test or a file failed, or no test file ran, either because there are none or because the filter matched none |
| `2` | The tests could not run: a usage error, a plugin that would not load, a build error in the project, or an invalid `HAPPYVIEW_WATCH_POLL_MS` under `--watch` |

A build error is reported the way `happyview check` reports it, with the file and the reason.

## Writing a test file

A test file is a script of its own kind. It defines no `handle`: it declares tests, and each one reports pass, fail or skip. Each interpreter brings its own assertion style, busted-style in Lua and Vitest-style in JavaScript and TypeScript, and the calls that reach HappyView come from the `internal.test` module, which is available only to a test file.

```lua
local test = require("internal.test")

describe("getThing", function()
  it("answers the thing", function()
    local res = test.query("com.example.getThing", { id = "1" }, { as = "did:plc:alice" })
    assert.are.equal(200, res.status)
  end)
end)
```

A test file can `require` the project's own modules by a path relative to itself, the same way a handler does, so `src/lib/format.test.lua` reaches `src/lib/format.lua` as `require("./format")` and can test it directly without firing anything.

An interpreter built before test runs existed refuses a test file. The run names the interpreter and says it needs `happyview-plugin-sdk` 0.3.0 or later; update the plugin to fix it.

## Firing triggers

Each trigger goes in through the same entry point the real event uses, so what runs is the handler as it runs in production, together with the lexicon validation, auth and indexing around it. Every trigger answers a `status` and a `body`.

- **`query` and `procedure`** send a request through HappyView's XRPC router in process, with no socket. Lexicon validation, parameter parsing and auth checks all apply. A query's input is its parameters, and a list becomes a repeated parameter. A procedure's input is its JSON body. The `body` is the response's JSON. A procedure fired with no caller is refused with `401`, as every procedure is, and one whose input carries `delegateDid` is refused with `400`: to act as another account, fire the trigger as that account.
- **`record_create`, `record_update` and `record_delete`** hand the record handler the event the firehose would have sent, so the collection's `record.*` scripts run and the record is indexed with a real CID. The collection needs a record lexicon in the project, or the call is refused naming it. The record's author is the `did` you give, else the caller, else `did:plc:test`. `record_create` makes up an rkey when you give none; `record_update` and `record_delete` need the rkey of the record they change. The `body` is `outcome`, `uri`, `cid` and `errors`.
- **`job`** creates a job of the type you name and runs its script once, to the end. The `body` is the job's `id`, `status`, `result`, `error` and `progress`, and the `status` is `200` when the job completed and `500` otherwise.
- **`label`** applies a label to the URI you name, as a labeler's event would, and runs the label script. The label is `val`, with optional `src`, `neg`, `cts` and `exp`; `src` defaults to `did:plc:labeler`. The `body` is `applied`, the label as stored or null, and `errors`.

A record or label script that fails makes the trigger's `status` `500`, with the error in `errors`. A production instance retries a failed record or label script before giving up; a test run tries it once and reports the failure.

### Acting as someone

`as` sets the caller a trigger runs as: `{ as = "did:plc:alice" }`. A forged caller exists only in a test run, and nothing arriving from outside can claim one.

A test run makes up identities rather than resolving them. The handle `alice.test` resolves to `did:plc:alice` and back, lowercased, so `Alice.test` is `did:plc:alice` too. Any other handle is refused with an error naming the `<name>.test` form.

## What stands in for the outside world

Nothing a test run does reaches the real network: no PLC directory, relay, Jetstream or PDS.

- **Each test file has its own database.** HappyView migrates one database at the start of the run, and each file starts from a fresh copy of it, so nothing one file writes is seen by another. The tests in one file share theirs, and run in order.
- **Test files run in parallel**, as many at a time as the machine has cores.
- **`ctx.env` comes from `.env.test`** in the project root, never from `.env`. `happyview test` does not load the project's `.env` at all, since that file holds the secrets of a real instance. Setting `HAPPYVIEW_SCRIPT_ENV_FILE` in the environment overrides `.env.test`.
- **The PDS is in process.** Writes made as the caller, whether by a scriptless procedure, by a script through `happyview.record` or by a linked repo, go to a PDS inside the test run. It answers `createRecord`, `putRecord`, `deleteRecord` and `uploadBlob` with real CIDs, and its writes are mirrored into the index as a real PDS's are. Any other PDS method answers `501`. It refuses a write to a repo other than the caller's, and it does not check `swapRecord`. `pds_writes()` returns the record writes the current test made; uploaded blobs are not among them.
- **Libraries can be answered from data.** `mock("http", { get = { status = 200 } })` answers every call to `get` with that response. A list of responses answers in order, and a call after the last one fails. A response of `{ ["$error"] = { code = "TIMEOUT", message = "slow" } }` makes the call fail with that code. Calling a method that the mock does not list fails, naming the methods it does. Mocking a namespace that no installed library serves is refused, since the real calls would still run. A namespace you do not mock runs for real.
- **Library calls are recorded.** `calls("http")` returns the calls the current test made to that namespace, each with its `method` and `args`, whether it was mocked or not.

A mock lasts for the rest of the file once it is set; set mocks in `before_each` when tests need different ones. Recorded calls and PDS writes start empty for each test.

No job worker runs during a test, so a job a script queues stays pending. Fire it with `job` to run it.

Some things a test run cannot do, and each refuses with an error saying it is not served in a test run:

- downloading a blob from an account's PDS
- writing to a space whose author's copy lives on their PDS rather than in HappyView, which answers `501`, or fetching a blob from such a space
- subscribing to or querying a labeler whose account was made up for the run

## Time limits

A test file may spend 60 seconds running its own code: building inputs, looping and asserting. Time spent inside a trigger does not count against it; each handler a trigger runs has the instance's usual script time limit, `script_wall_clock_seconds` (10 seconds by default). A file stopped by its limit fails, and the other files carry on.

## Watching

`--watch` runs the tests, then waits for changes to the project and reruns:

- A change to test files only reruns those files.
- Any other change, to a handler, a module, a lexicon or `happyview.toml`, reruns every test file, because which handlers a test reaches is decided by the triggers it fires as it runs.
- A change to `happyview.toml` or `.env.test` is loaded before the rerun.
- A save that breaks the build prints the error and keeps watching. The changes it held are rerun along with the next save that builds.

A save made while tests are running waits for the run to finish. `HAPPYVIEW_WATCH_POLL_MS` switches the watch to polling, as it does for [`happyview dev`](../getting-started/configuration.md#running-locally-happyview-dev).
