---
title: "Architecture"
---

Guide for contributors working on HappyView itself. For a user-facing overview, see the [Introduction](../index.md).

## System overview

```mermaid
graph LR
  Application

  Application -->|"GET /xrpc/{method}"| Query
  Application -->|"POST /xrpc/{method}"| Procedure

  subgraph HappyView
    Query["Query Handler<br/><small>Lua Script (Optional)</small>"]
    Procedure["Procedure Handler<br/><small>Lua Script (Optional)</small>"]
    Spaces["Spaces<br/><small>Permissioned Data</small>"]
  end

  Procedure --> DB
  Query --> DB
  Spaces --> DB

  Procedure -->|proxy write| PDS["User PDS"]

  DB[("SQLite / PostgreSQL<br/><small>records · lexicons · spaces</small>")]

  Jetstream["Jetstream<br/><small>WebSocket</small>"] -->|record events| DB
  Relay["Relay<br/><small>listReposByCollection</small>"] -->|repo discovery| Backfill
  Backfill["Backfill Worker"] -->|listRecords| PDS
  Backfill --> DB

  Labeler["Labeler<br/><small>WebSocket (out-of-band)</small>"] -->|label events| DB
```

Queries go through the query handler to the database (SQLite by default, or Postgres). Writes go through the procedure handler to the user's PDS, then HappyView indexes the record locally. Real-time record events stream in via [Jetstream](https://github.com/bluesky-social/jetstream); historical records are backfilled in-process by discovering repos via the relay's `listReposByCollection` and fetching records directly from each PDS. [Labelers](../guides/labelers.md) are external services that emit content labels over a direct WebSocket connection — they operate out-of-band, outside the relay/repo system. [Spaces](../experimental/spaces/index.md) provide permissioned data containers with membership-gated access, per-user repo state tracking (LtHash + signed commits), and cross-service credential-based authentication.

## Request flow

### Reads (queries)

```mermaid
sequenceDiagram
    participant C as Client
    participant X as xrpc_get()
    participant R as LexiconRegistry
    participant L as Lua Script
    participant D as Database

    C->>X: GET /xrpc/{method}?params
    X->>R: Lookup (must be Query type)
    alt Lua script attached
        R->>L: Execute script
        L->>D: db.query / db.get / db.raw
        D-->>L: Results
        L-->>X: Response table
    else No script
        R->>D: Default SQL query (collection from target_collection)
        D-->>X: Results
    end
    X-->>C: JSON response
```

### Writes (procedures)

```mermaid
sequenceDiagram
    participant C as Client
    participant A as Claims Extractor
    participant X as xrpc_post()
    participant R as LexiconRegistry
    participant L as Lua Script
    participant S as OAuth Session
    participant P as User PDS
    participant D as Database

    C->>A: POST /xrpc/{method} + DPoP auth + X-Client-Key
    A->>X: Validated claims
    X->>R: Lookup (must be Procedure type)
    alt Lua script attached
        R->>L: Execute script (Record API)
        L->>S: Record:save()
    else No script
        R->>S: Default create/update (auto-detect from uri field)
    end
    S->>P: Proxy write (createRecord or putRecord)
    P-->>S: PDS response
    S->>D: Upsert record locally
    S-->>C: Forward PDS response
```

### Admin endpoints

```mermaid
sequenceDiagram
    participant C as Client
    participant A as AdminAuth Extractor
    participant U as Users Table
    participant H as Admin Handler
    participant D as Database

    C->>A: Request + Bearer token
    A->>A: Validate claims (API key or service auth JWT)
    A->>U: DID lookup
    alt Users table empty
        U-->>A: Auto-bootstrap as super user
    else User found
        U-->>A: Load permissions
    end
    A->>A: Permission check (403 if missing)
    A->>H: Authorized request
    H->>D: Database operation
    D-->>H: Result
    H-->>C: JSON response
```

## Data flow

### Real-time indexing

```mermaid
sequenceDiagram
    participant J as Jetstream WebSocket
    participant H as HappyView
    participant D as Database
    participant R as LexiconRegistry

    H->>J: Connect (collection filters from indexed lexicons)
    loop Stream events
        J->>H: Record commit event
        alt create / update
            H->>D: UPSERT into records table
        else delete
            H->>D: DELETE from records table
        end
    end
    J->>H: Lexicon schema event (com.atproto.lexicon.schema)
    H->>D: Update tracked network lexicons
    H->>R: Update in-memory registry
    Note over H,D: Cursor persisted to instance_settings for resume on reconnect
    Note over H,J: Reconnects on collection filter changes (lexicon add/remove)
```

### Backfill

```mermaid
sequenceDiagram
    participant A as Admin
    participant H as HappyView
    participant D as Database
    participant Relay as Relay
    participant PLC as PLC Directory
    participant PDS as User PDS

    A->>H: POST /admin/backfill
    H->>D: Create backfill_jobs record (status = running)

    rect rgb(240, 248, 255)
    note over H,Relay: Phase 1: Discovery
    H->>Relay: listReposByCollection (paginated)
    Relay-->>H: List of DIDs
    H->>D: Insert backfill_repos rows
    end

    rect rgb(240, 255, 240)
    note over H,PDS: Phase 2: Pipelined resolve + fetch (concurrent)
    par Resolver task
        loop Unresolved DIDs
            H->>PLC: Resolve DID document
            PLC-->>H: PDS endpoint
            H->>D: Update backfill_repos.pds_endpoint
        end
    and Fetcher task
        loop Resolved DIDs (as they arrive)
            H->>PDS: listRecords (paginated)
            PDS-->>H: Records
            H->>D: UPSERT each record
        end
    end
    end

    H->>D: Mark job completed (or failed)
```

## Database schema

### `records`

| Column       | Type        | Description                         |
| ------------ | ----------- | ----------------------------------- |
| `uri`        | text (PK)   | AT URI (`at://did/collection/rkey`) |
| `did`        | text        | Author DID                          |
| `collection` | text        | Lexicon NSID                        |
| `rkey`       | text        | Record key                          |
| `record`     | jsonb       | Record value                        |
| `cid`        | text        | Content identifier                  |
| `indexed_at` | timestamptz | When HappyView indexed this record  |

### `lexicons`

| Column              | Type        | Description                                     |
| ------------------- | ----------- | ----------------------------------------------- |
| `id`                | text (PK)   | Lexicon NSID                                    |
| `revision`          | integer     | Incremented on upsert                           |
| `lexicon_json`      | jsonb       | Raw lexicon definition                          |
| `lexicon_type`      | text        | record, query, procedure, definitions           |
| `backfill`          | boolean     | Whether to backfill on upload                   |
| `target_collection` | text        | For queries/procedures: which record collection |
| `created_at`        | timestamptz |                                                 |
| `updated_at`        | timestamptz |                                                 |

### `users`

| Column         | Type          | Description                                      |
| -------------- | ------------- | ------------------------------------------------ |
| `id`           | uuid (PK)     |                                                  |
| `did`          | text (unique) | User's atproto DID                           |
| `is_super`     | boolean       | Whether this is the super user (only one allowed)|
| `created_at`   | timestamptz   |                                                  |
| `last_used_at` | timestamptz   | Updated on each authenticated request            |

### `user_permissions`

| Column       | Type        | Description                                  |
| ------------ | ----------- | -------------------------------------------- |
| `user_id`    | uuid (FK)   | References `users.id`                        |
| `permission` | text        | Permission string (e.g. `lexicons:create`)   |
| (PK)         |             | Composite primary key: (`user_id`, `permission`) |

### `api_keys`

| Column       | Type        | Description                                  |
| ------------ | ----------- | -------------------------------------------- |
| `id`         | uuid (PK)   |                                              |
| `user_id`    | uuid (FK)   | References `users.id`                        |
| `name`       | text        | Descriptive label                            |
| `key_hash`   | text        | SHA-256 hash of the full key                 |
| `key_prefix` | text        | First 11 characters for display              |
| `permissions`| text[]      | Permissions granted to this key              |
| `created_at` | timestamptz |                                              |
| `last_used_at`| timestamptz|                                              |
| `revoked_at` | timestamptz | Set when revoked (soft delete)               |

### `oauth_sessions`

| Column         | Type        | Description                                  |
| -------------- | ----------- | -------------------------------------------- |
| `did`          | text (PK)   | User's atproto DID                       |
| `session_data` | text        | Serialized OAuth session (managed by atrium) |
| `created_at`   | timestamptz |                                              |
| `updated_at`   | timestamptz |                                              |

### `oauth_state`

| Column       | Type        | Description                                  |
| ------------ | ----------- | -------------------------------------------- |
| `state_key`  | text (PK)   | OAuth state parameter                        |
| `state_data` | text        | Serialized state (managed by atrium)         |
| `created_at` | timestamptz |                                              |

### `instance_settings`

| Column       | Type        | Description                                  |
| ------------ | ----------- | -------------------------------------------- |
| `key`        | text (PK)   | Setting name (e.g. `app_name`)               |
| `value`      | text        | Setting value                                |
| `updated_at` | timestamptz | Last modified                                |

### `event_logs`

| Column       | Type        | Description                                  |
| ------------ | ----------- | -------------------------------------------- |
| `id`         | uuid (PK)   |                                              |
| `event_type` | text        | Category.action format (e.g. `user.created`) |
| `severity`   | text        | `info`, `warn`, or `error`                   |
| `actor_did`  | text        | DID of the user who triggered the event      |
| `subject`    | text        | What was affected (DID, NSID, URI, etc.)     |
| `detail`     | jsonb       | Event-specific data                          |
| `created_at` | timestamptz |                                              |

### `script_variables`

| Column       | Type        | Description                                  |
| ------------ | ----------- | -------------------------------------------- |
| `key`        | text (PK)   | Variable name                                |
| `value`      | text        | Variable value (encrypted at rest)           |
| `created_at` | timestamptz |                                              |
| `updated_at` | timestamptz |                                              |

### `spaces`

| Column            | Type        | Description                                      |
| ----------------- | ----------- | ------------------------------------------------ |
| `id`              | text (PK)   | Internal space identifier                        |
| `did`             | text        | The space's own DID                              |
| `authority_did`   | text        | DID that controls the space                      |
| `creator_did`     | text        | DID of the user who created the space            |
| `type_nsid`       | text        | Space type as an NSID                            |
| `skey`            | text        | Space key (differentiates spaces of the same type) |
| `display_name`    | text        | Human-readable name (optional)                   |
| `description`     | text        | Description (optional)                           |
| `mint_policy`     | text        | `member-list`, `public`, or `managing-app`       |
| `app_access`      | text (JSON) | `{"type":"open"}` or `{"type":"allowList","allowed":[...]}` |
| `managing_app_did`| text        | DID of the managing app (optional)               |
| `config`          | text (JSON) | Space config (`membershipPublic`, `recordsPublic`, extras) |
| `revision`        | text        | Current revision TID                             |
| `created_at`      | text        |                                                  |
| `updated_at`      | text        |                                                  |

### `space_members`

| Column         | Type        | Description                                      |
| -------------- | ----------- | ------------------------------------------------ |
| `id`           | text (PK)   |                                                  |
| `space_id`     | text (FK)   | References `spaces.id`                           |
| `did`          | text        | Member's DID (or space URI for delegation)       |
| `access`       | text        | `read`, `read_self`, or `write`                  |
| `is_delegation`| boolean     | Whether this member is a delegated space         |
| `granted_by`   | text        | DID of who granted membership                    |
| `created_at`   | text        |                                                  |

### `space_records`

| Column         | Type        | Description                                      |
| -------------- | ----------- | ------------------------------------------------ |
| `uri`          | text (PK)   | `at://` URI of the record                        |
| `space_id`     | text (FK)   | References `spaces.id`                           |
| `author_did`   | text        | DID of the record author                         |
| `collection`   | text        | Lexicon NSID                                     |
| `rkey`         | text        | Record key                                       |
| `record`       | jsonb       | Record value                                     |
| `cid`          | text        | Content identifier                               |
| `indexed_at`   | text        |                                                  |

### `space_repo_state`

| Column         | Type        | Description                                      |
| -------------- | ----------- | ------------------------------------------------ |
| `id`           | text (PK)   |                                                  |
| `space_id`     | text (FK)   | References `spaces.id`                           |
| `author_did`   | text        | DID of the repo author                           |
| `lthash_state` | bytea       | 2048-byte LtHash state                           |
| `rev`          | text        | Current revision                                 |
| `hash`         | bytea       | Content hash                                     |
| `ikm`          | bytea       | Input keying material for deniable signatures    |
| `sig`          | bytea       | Signature                                        |
| `mac`          | bytea       | Message authentication code                      |
| `updated_at`   | text        |                                                  |

### `space_record_oplog`

| Column         | Type        | Description                                      |
| -------------- | ----------- | ------------------------------------------------ |
| `id`           | text (PK)   |                                                  |
| `space_id`     | text (FK)   | References `spaces.id`                           |
| `author_did`   | text        | DID of the operation author                      |
| `rev`          | text        | Revision this operation belongs to               |
| `idx`          | integer     | Index within the revision                        |
| `action`       | text        | `create`, `update`, or `delete`                  |
| `collection`   | text        | Lexicon NSID                                     |
| `rkey`         | text        | Record key                                       |
| `cid`          | text        | Content identifier (for create/update)           |
| `prev`         | text        | Previous CID (for update/delete)                 |
| `created_at`   | text        |                                                  |

### `space_notify_registrations`

| Column         | Type        | Description                                      |
| -------------- | ----------- | ------------------------------------------------ |
| `id`           | text (PK)   |                                                  |
| `space_id`     | text (FK)   | References `spaces.id`                           |
| `author_did`   | text        | Filter by author DID (optional)                  |
| `endpoint`     | text        | Notification endpoint URL                        |
| `registered_by`| text        | DID of who registered                            |
| `expires_at`   | text        | When the registration expires                    |
| `created_at`   | text        |                                                  |

### `space_invites`

| Column       | Type      | Description                                      |
| ------------ | --------- | ------------------------------------------------ |
| `id`         | text (PK) |                                                  |
| `space_id`   | text (FK) | References `spaces.id`                           |
| `token_hash` | text      | SHA-256 hash of the invite token                 |
| `created_by` | text      | DID of the user who created the invite           |
| `access`     | text      | Access level granted: `read`, `read_self`, `write` |
| `max_uses`   | integer?  | Maximum number of uses (null = unlimited)        |
| `uses`       | integer   | Current use count                                |
| `expires_at` | text?     | Expiry timestamp (null = never)                  |
| `revoked`    | boolean   | Whether the invite has been revoked              |
| `created_at` | text      |                                                  |

### `space_credentials`

| Column       | Type      | Description                                      |
| ------------ | --------- | ------------------------------------------------ |
| `id`         | text (PK) |                                                  |
| `space_id`   | text (FK) | References `spaces.id`                           |
| `issued_to`  | text      | DID the credential was issued to                 |
| `token_hash` | text      | Hash of the credential token                     |
| `expires_at` | text      | When the credential expires                      |
| `created_at` | text      |                                                  |

### `space_dids`

| Column             | Type      | Description                                      |
| ------------------ | --------- | ------------------------------------------------ |
| `id`               | text (PK) |                                                  |
| `did`              | text      | The space's DID                                  |
| `space_id`         | text (FK) | References `spaces.id`                           |
| `signing_key_enc`  | text      | Encrypted signing key (AES-256-GCM)              |
| `rotation_key_enc` | text      | Encrypted rotation key (AES-256-GCM)             |
| `created_by`       | text      | DID of who provisioned the key                   |
| `created_at`       | text      |                                                  |

### `service_identity`

| Column                | Type        | Description                                      |
| --------------------- | ----------- | ------------------------------------------------ |
| `id`                  | integer (PK)| Always 1 (singleton)                             |
| `mode`                | text        | `did_web`, `did_plc`, or `linked_account`        |
| `did`                 | text        | The service's DID                                |
| `signing_key_enc`     | text        | Encrypted signing key                            |
| `rotation_key_enc`    | text?       | Encrypted rotation key (did:plc only)            |
| `attached_account_did`| text?       | Linked account DID (linked_account mode)         |
| `setup_complete`      | boolean     | Whether setup has been finalized                 |
| `created_at`          | text        |                                                  |
| `updated_at`          | text        |                                                  |

### `service_entries`

| Column        | Type        | Description                                      |
| ------------- | ----------- | ------------------------------------------------ |
| `id`          | integer (PK)|                                                  |
| `fragment_id` | text        | DID document fragment identifier                 |
| `service_type`| text        | Service type (e.g. `AtprotoAppView`)             |
| `access_mode` | text        | `all` or scoped to specific XRPCs                |
| `created_at`  | text        |                                                  |
| `updated_at`  | text        |                                                  |

### `service_entry_xrpcs`

| Column             | Type        | Description                                      |
| ------------------ | ----------- | ------------------------------------------------ |
| `service_entry_id` | integer (FK)| References `service_entries.id`                  |
| `lexicon_id`       | text        | Lexicon NSID this entry handles                  |

### `verification_methods`

| Column                  | Type      | Description                                      |
| ----------------------- | --------- | ------------------------------------------------ |
| `id`                    | text (PK) |                                                  |
| `fragment_id`           | text      | DID document fragment (e.g. `#atproto_space`)    |
| `key_type`              | text      | Always `Multikey`                                |
| `public_key_multibase`  | text      | Public key in multibase encoding                 |
| `private_key_enc`       | text      | Encrypted private key (AES-256-GCM)              |
| `created_at`            | text      |                                                  |

### `backfill_jobs`

| Column            | Type        | Description                                              |
| ----------------- | ----------- | -------------------------------------------------------- |
| `id`              | uuid (PK)   |                                                          |
| `collection`      | text        | Target collection (null = all)                           |
| `did`             | text        | Target DID (null = all)                                  |
| `status`          | text        | pending, running, pausing, paused, cancelling, cancelled, completed, failed |
| `stage`           | text        | pending, discovering_repos, resolving_and_fetching, completed, failed, cancelled |
| `total_repos`     | integer     | Total DIDs discovered                                    |
| `resolved_repos`  | integer     | DIDs with PDS endpoint resolved                          |
| `processed_repos` | integer     | DIDs with records fetched                                |
| `total_records`   | integer     | Total records indexed                                    |
| `error`           | text        | Error message if failed                                  |
| `started_at`      | timestamptz |                                                          |
| `completed_at`    | timestamptz |                                                          |
| `created_at`      | timestamptz |                                                          |

## Testing

```sh
# Unit tests (no database needed)
cargo test --lib

# All tests including end-to-end (SQLite by default)
cargo test

# Or run against Postgres
docker compose -f docker-compose.test.yml up -d
TEST_DATABASE_URL=postgres://happyview:happyview@localhost:5433/happyview_test cargo test
docker compose -f docker-compose.test.yml down
```

End-to-end tests use `wiremock` to mock external services (PLC directory, PDSes) and a real database for full integration coverage. By default tests use SQLite; set `TEST_DATABASE_URL` to a Postgres connection string to test against Postgres.
