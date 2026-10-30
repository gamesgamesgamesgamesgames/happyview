---
title: "Project"
---

Admin API endpoints for deploying a project: the `happyview.toml`, `src/` and compiled modules `hpy deploy` sends. A deploy is built by the same loader that builds `HAPPYVIEW_PROJECT` at boot, so a bundle that deploys is one that would boot. Both endpoints need `project:deploy`.

```sh
# All examples assume $TOKEN is an API key (hv_...)
AUTH="Authorization: Bearer $TOKEN"
```

A deployed project is fully owned by the deploy: a lexicon or script removed from the bundle is removed from the instance. Lexicons and scripts created in the dashboard are never touched. A project file with the same id as a dashboard one answers in its place, with a warning, and removing the file lets the dashboard one answer again.

An instance started with `HAPPYVIEW_PROJECT` takes its project from that directory, and both endpoints answer `409` with `{"error": "ProjectFromFile"}`.

## Deploy a project

```
PUT /admin/project?dry_run=true|false
```

The body is a tar.gz (`Content-Type: application/gzip`) of paths relative to the project root:

- `happyview.toml`
- files under `src/`; tests are accepted and never served
- `.happyview/build/<name>.wasm` for each `[modules.<name>]`

Anything else, an absolute path, a `..` component, a link, or a path given twice is refused with `422` and nothing is applied. So is a bundle that expands past 200 MiB or holds more than 10,000 entries. A body over 50 MiB is refused with `413`.

The bundle is built against the plugins installed now, so install the project's interpreters and libraries first. With `dry_run=true` everything is checked and reported and nothing is stored or served. Otherwise the bundle is stored in the database and served in one atomic swap; requests already running finish on the project they started with. The stored bundle is served again at every boot.

Send `If-Match` with the `sha256` from [`GET /admin/project`](#get-the-deployed-project) to make sure a deploy only replaces the project you saw. A mismatch answers `412`. Without the header the deploy replaces whatever is there.

```sh
curl -X PUT "http://localhost:3000/admin/project?dry_run=true" \
  -H "$AUTH" -H "Content-Type: application/gzip" \
  -H 'If-Match: "9f2c…"' \
  --data-binary @project.tar.gz
```

**Response**: `200 OK`

```json
{
  "sha256": "4b1e…",
  "dry_run": false,
  "deployed_at": "2026-10-28T18:04:11.502+00:00",
  "changes": {
    "lexicons": { "added": ["com.example.post"], "changed": [], "removed": [] },
    "scripts": { "added": ["xrpc.query:com.example.listPosts"], "changed": [], "removed": [] },
    "modules": { "added": ["src/lib/format.lua", "rank"], "changed": [], "removed": [] },
    "network_lexicons": { "added": [], "changed": [], "removed": [] },
    "labelers": { "added": [], "changed": [], "removed": [] }
  },
  "backfills": ["com.example.post"],
  "warnings": [
    { "path": "happyview.toml", "message": "[instance] is ignored by a deploy; it configures the instance, not the project" }
  ]
}
```

| Field         | Description                                                                                                         |
| ------------- | ------------------------------------------------------------------------------------------------------------------- |
| `sha256`      | Hex SHA-256 of the request body                                                                                     |
| `deployed_at` | When the project now served was deployed; `null` on a dry run. Redeploying the same bytes keeps the original time |
| `changes`     | What the deploy changes, compared with the project being served. Each kind has `added`, `changed` and `removed`   |
| `backfills`   | Collections whose backfill started, or on a dry run would start                                                   |
| `warnings`    | Problems that did not stop the deploy, each with `path`, `message` and sometimes `line`                           |

`changes` keys:

| Kind               | Keyed by                                                                                                             |
| ------------------ | -------------------------------------------------------------------------------------------------------------------- |
| `lexicons`         | NSID                                                                                                                 |
| `scripts`          | Trigger id, for every kind of trigger                                                                                |
| `modules`          | Project path for a shared source module (`src/lib/format.lua`), name for a compiled `[modules.<name>]` (`rank`)   |
| `network_lexicons` | NSID in `[network_lexicons] include`. Never `changed`; a removed one is no longer declared and stays as a dashboard row |
| `labelers`         | DID in `[labels] subscribe`, on the same terms                                                                      |

A backfill starts only for a record lexicon declared with `[lexicons."<nsid>"] backfill = true`, only if it is indexed, and only if its collection has never been backfilled on this instance, which is the rule `HAPPYVIEW_PROJECT` follows at boot. `project:deploy` is enough to start it.

`[instance]`, `[dev]` and `[jetstream]` configure the instance rather than the project. A deploy ignores them and says so in `warnings`. `[interpreters]`, `[libraries]` and `[happyview] version` are ignored without a warning: the plugins have to be installed before the deploy, and the deploying client checks the version.

### Errors

| Status | `error`              | When                                                                                     |
| ------ | -------------------- | ---------------------------------------------------------------------------------------- |
| 403    | `InsufficientPermissions` | The caller lacks `project:deploy`; the message names it                             |
| 409    | `ProjectFromFile`    | The instance serves `HAPPYVIEW_PROJECT`                                                  |
| 412    | `PreconditionFailed` | `If-Match` does not name the deployed project. `current` holds its `sha256`, or `null` |
| 413    | `BundleTooLarge`     | The body is over 50 MiB                                                                  |
| 422    | `InvalidProject`     | The bundle is unsafe or the project does not build. Nothing was applied                 |

A `422` lists every problem found, not only the first:

```json
{
  "error": "InvalidProject",
  "message": "the project did not build, so nothing was applied",
  "problems": [
    {
      "path": "src/lexicons/com/example/post.json",
      "message": "the lexicon's id is 'com.example.wrong' but its path names 'com.example.post'; move the file to src/lexicons/com/example/wrong.json or change the id"
    }
  ]
}
```

## Get the deployed project

```
GET /admin/project
```

```sh
curl http://localhost:3000/admin/project -H "$AUTH"
```

**Response**: `200 OK`

```json
{
  "sha256": "4b1e…",
  "deployed_at": "2026-10-28T18:04:11.502+00:00",
  "deployed_by": "did:plc:abc123"
}
```

When the stored project did not build at boot, for example because an interpreter it needs was uninstalled, the instance still starts but serves no project, and the response carries the reason. Deploying again, after fixing what the problems name, clears it:

```json
{
  "sha256": "4b1e…",
  "deployed_at": "2026-10-28T18:04:11.502+00:00",
  "deployed_by": "did:plc:abc123",
  "error": {
    "message": "the deployed project did not build when the instance started, so none is being served",
    "problems": [
      {
        "path": "src/lexicons/com/example/listPosts.lua",
        "message": "no installed interpreter runs .lua files (no interpreter is installed); install one that does, or rename the file"
      }
    ]
  }
}
```

Nothing deployed yet answers `404` with `{"error": "NoProject"}`.

A deploy reaches the instance that received it. Other instances sharing the database serve it from their next restart.
