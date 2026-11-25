-- PDSes that refused a space URI in Proposal 0016's `at:space/` form (Nov 24,
-- 2026) and took the `at://…/space/…` form before it. HappyView sends
-- `at:space/` to every other host. Keyed by endpoint, as
-- happyview_space_pds_support is: the form is a property of the server.
CREATE TABLE happyview_space_host_uri_forms (
    pds_endpoint TEXT PRIMARY KEY,
    recorded_at TEXT NOT NULL
);
