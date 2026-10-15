---
title: Native binary
---

Every release publishes a self-contained `happyview` binary for macOS (Apple
silicon and Intel), Linux (x86_64 and arm64, glibc and static musl) and
Windows (x86_64). It carries its dashboard and its database migrations, so it
needs nothing beside it but a database.

Download `happyview-<target>.tar.xz` (`.zip` on Windows) from the
[releases page](https://github.com/gamesgamesgamesgamesgames/happyview/releases),
check it against the `.sha256` file published beside it, and extract it. On
Linux, the `musl` build runs on any distribution; the `gnu` build needs glibc
2.35 or later.

```bash
DATABASE_URL="sqlite://$PWD/data/happyview.db?mode=rwc" \
PUBLIC_URL=http://127.0.0.1:3000 \
SESSION_SECRET="$(openssl rand -base64 48)" \
./happyview
```

The dashboard is served from inside the binary, and `BASE_PATH` is applied to
it as it is served. Set `STATIC_DIR` to serve a different build from disk
instead. The server does not rewrite files read from a directory, so that
build has to be exported for the subpath you serve it under; the Docker image
does this itself at startup, which is why `BASE_PATH` works there. A
`STATIC_DIR` that is set but empty counts as unset.

On Windows, set the variables with `$env:DATABASE_URL = "..."` in PowerShell
before running `.\happyview.exe`.

Set `PLUGIN_REGISTRY_URL=off` on an instance with no outbound network to
stop it fetching the official plugin catalogue.
