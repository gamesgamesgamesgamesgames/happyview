-- Retention holds protected events back to the oldest stored grant's creation.
CREATE INDEX idx_space_access_grants_created_at ON happyview_space_access_grants(created_at);
