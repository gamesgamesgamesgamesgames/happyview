-- A plugin installed from the registry is fetched by CID: these record which
-- project, channel and version it is, where the registry serves its manifest
-- and module, and the CIDs both were verified against, so a boot reload can
-- fetch and verify the same bytes again. All NULL for a plugin installed from
-- a plain URL or a file. `registry_unresolved` marks a URL install that could
-- not be matched to a registry project, so the match is not retried on every
-- refresh. Nullable columns, and a constant default, rewrite nothing.
ALTER TABLE happyview_plugins ADD COLUMN IF NOT EXISTS manifest_url TEXT;
ALTER TABLE happyview_plugins ADD COLUMN IF NOT EXISTS manifest_blob_cid TEXT;
ALTER TABLE happyview_plugins ADD COLUMN IF NOT EXISTS package_cid TEXT;
ALTER TABLE happyview_plugins ADD COLUMN IF NOT EXISTS registry_did TEXT;
ALTER TABLE happyview_plugins ADD COLUMN IF NOT EXISTS registry_name TEXT;
ALTER TABLE happyview_plugins ADD COLUMN IF NOT EXISTS registry_channel TEXT;
ALTER TABLE happyview_plugins ADD COLUMN IF NOT EXISTS version_uri TEXT;
ALTER TABLE happyview_plugins ADD COLUMN IF NOT EXISTS manifest_cid TEXT;
ALTER TABLE happyview_plugins ADD COLUMN IF NOT EXISTS registry_unresolved INTEGER NOT NULL DEFAULT 0;
