-- Whether `value` holds ciphertext. A flag rather than a prefix on the value,
-- because any prefix is also text a variable could legitimately hold. Rows are
-- encrypted by `script::variables::encrypt_stored` at boot, since SQL has no
-- key.
ALTER TABLE happyview_script_variables ADD COLUMN encrypted INTEGER NOT NULL DEFAULT 0;
