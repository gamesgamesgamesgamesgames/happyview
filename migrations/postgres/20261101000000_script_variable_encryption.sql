-- Whether `value` holds ciphertext. A flag rather than a prefix on the value,
-- because any prefix is also text a variable could legitimately hold. Rows are
-- encrypted by `script::variables::encrypt_stored` at boot, since SQL has no
-- key. INTEGER rather than BOOLEAN on both backends, as the space tables do.
ALTER TABLE happyview_script_variables ADD COLUMN IF NOT EXISTS encrypted INTEGER NOT NULL DEFAULT 0;
