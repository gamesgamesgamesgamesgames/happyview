-- How a registration is delivered. `webhook` registrations name a full URL and
-- receive the legacy payload; `xrpc` registrations name a service identifier
-- and receive `com.atproto.space.notifyWrite` at its endpoint.
ALTER TABLE happyview_space_notify_registrations ADD COLUMN delivery TEXT NOT NULL DEFAULT 'webhook';
