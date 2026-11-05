-- The registry account whose catalogue entry a plugin was installed from, so
-- an update is offered only from that same publisher: anyone can publish any
-- slug, and the catalogue holds one entry per id. NULL for a plugin installed
-- from a URL the catalogue did not offer and for rows written before this
-- column existed; those fall back to judging the stored URL.
ALTER TABLE happyview_plugins ADD COLUMN publisher_did TEXT;
