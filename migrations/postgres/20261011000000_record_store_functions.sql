-- SQL-side codecs for the compact record format (Phase 3). The compatibility
-- views render cid and timestamps back to their old text forms with these
-- (indexed_at and created_at are TEXT on both backends), and
-- their INSTEAD OF triggers encode raw-SQL writes. Same rules as
-- src/record_store/codec.rs and the SQLite functions in
-- src/record_store/sqlite_functions.rs; tests there hold the three to
-- agreement. The Rust codec is the definition. Timestamps are computed in
-- integer arithmetic, so neither the session time zone nor Postgres's own
-- date parsing (which reads 'now', 'epoch' and offset-less text in the
-- session zone) comes into them.

CREATE OR REPLACE FUNCTION hv_base32_encode(b bytea) RETURNS text
LANGUAGE plpgsql IMMUTABLE STRICT PARALLEL SAFE AS $$
DECLARE
    alphabet CONSTANT text := 'abcdefghijklmnopqrstuvwxyz234567';
    out text := '';
    buffer bigint := 0;
    bits int := 0;
    i int;
BEGIN
    FOR i IN 0 .. length(b) - 1 LOOP
        buffer := (buffer << 8) | get_byte(b, i);
        bits := bits + 8;
        WHILE bits >= 5 LOOP
            out := out || substr(alphabet, ((buffer >> (bits - 5)) & 31)::int + 1, 1);
            bits := bits - 5;
        END LOOP;
        buffer := buffer & ((1::bigint << bits) - 1);
    END LOOP;
    IF bits > 0 THEN
        out := out || substr(alphabet, ((buffer << (5 - bits)) & 31)::int + 1, 1);
    END IF;
    RETURN out;
END $$;

-- Leftover bits are dropped, so a caller that needs canonical input
-- re-encodes and compares. NULL for a character outside the alphabet.
CREATE OR REPLACE FUNCTION hv_base32_decode(t text) RETURNS bytea
LANGUAGE plpgsql IMMUTABLE STRICT PARALLEL SAFE AS $$
DECLARE
    alphabet CONSTANT text := 'abcdefghijklmnopqrstuvwxyz234567';
    out bytea := '\x'::bytea;
    buffer bigint := 0;
    bits int := 0;
    i int;
    v int;
BEGIN
    FOR i IN 1 .. length(t) LOOP
        v := strpos(alphabet, substr(t, i, 1)) - 1;
        IF v < 0 THEN
            RETURN NULL;
        END IF;
        buffer := (buffer << 5) | v;
        bits := bits + 5;
        IF bits >= 8 THEN
            out := out || set_byte('\x00'::bytea, 0, ((buffer >> (bits - 8)) & 255)::int);
            bits := bits - 8;
            buffer := buffer & ((1::bigint << bits) - 1);
        END IF;
    END LOOP;
    RETURN out;
END $$;

-- A canonical base32 CIDv1 ('b' and the base32 of bytes starting 0x01) as
-- its binary form; '' as no bytes; any other text behind a 0x00 marker byte.
CREATE OR REPLACE FUNCTION hv_cid_bytes(t text) RETURNS bytea
LANGUAGE plpgsql IMMUTABLE STRICT PARALLEL SAFE AS $$
DECLARE
    decoded bytea;
BEGIN
    IF t = '' THEN
        RETURN '\x'::bytea;
    END IF;
    IF left(t, 1) = 'b' THEN
        decoded := hv_base32_decode(substr(t, 2));
        IF decoded IS NOT NULL AND length(decoded) > 0 AND get_byte(decoded, 0) = 1
           AND 'b' || hv_base32_encode(decoded) = t THEN
            RETURN decoded;
        END IF;
    END IF;
    RETURN '\x00'::bytea || convert_to(t, 'UTF8');
END $$;

-- Text behind the marker must be UTF-8 here; Rust decodes invalid bytes
-- lossily, but hv_cid_bytes never writes them.
CREATE OR REPLACE FUNCTION hv_cid_text(b bytea) RETURNS text
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS $$
    SELECT CASE
        WHEN length(b) = 0 THEN ''
        WHEN get_byte(b, 0) = 0 THEN convert_from(substr(b, 2), 'UTF8')
        ELSE 'b' || hv_base32_encode(b)
    END
$$;

-- Days from 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's
-- days_from_civil). The date must already be valid.
CREATE OR REPLACE FUNCTION hv_days_from_civil(y bigint, m bigint, d bigint) RETURNS bigint
LANGUAGE plpgsql IMMUTABLE STRICT PARALLEL SAFE AS $$
DECLARE
    yy bigint := y - CASE WHEN m <= 2 THEN 1 ELSE 0 END;
    era bigint;
    yoe bigint;
    doy bigint;
    doe bigint;
BEGIN
    era := (CASE WHEN yy >= 0 THEN yy ELSE yy - 399 END) / 400;
    yoe := yy - era * 400;
    doy := (153 * (m + CASE WHEN m > 2 THEN -3 ELSE 9 END) + 2) / 5 + d - 1;
    doe := yoe * 365 + yoe / 4 - yoe / 100 + doy;
    RETURN era * 146097 + doe - 719468;
END $$;

-- Unix microseconds as RFC 3339 in UTC with six fractional digits, years
-- 0000 to 9999 (Hinnant's civil_from_days). Anything outside cannot have
-- been written by HappyView and renders as 'invalid timestamp <us>'.
CREATE OR REPLACE FUNCTION hv_ts_text(us bigint) RETURNS text
LANGUAGE plpgsql IMMUTABLE STRICT PARALLEL SAFE AS $$
DECLARE
    days bigint;
    rem bigint;
    z bigint;
    era bigint;
    doe bigint;
    yoe bigint;
    doy bigint;
    mp bigint;
    y bigint;
    m bigint;
    d bigint;
BEGIN
    IF us < -62167219200000000 OR us > 253402300799999999 THEN
        RETURN 'invalid timestamp ' || us;
    END IF;
    days := us / 86400000000;
    rem := us % 86400000000;
    IF rem < 0 THEN
        days := days - 1;
        rem := rem + 86400000000;
    END IF;
    z := days + 719468;
    era := (CASE WHEN z >= 0 THEN z ELSE z - 146096 END) / 146097;
    doe := z - era * 146097;
    yoe := (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    doy := doe - (365 * yoe + yoe / 4 - yoe / 100);
    mp := (5 * doy + 2) / 153;
    d := doy - (153 * mp + 2) / 5 + 1;
    m := CASE WHEN mp < 10 THEN mp + 3 ELSE mp - 9 END;
    y := yoe + era * 400 + CASE WHEN m <= 2 THEN 1 ELSE 0 END;
    RETURN lpad(y::text, 4, '0') || '-' || lpad(m::text, 2, '0') || '-' || lpad(d::text, 2, '0')
        || 'T' || lpad((rem / 3600000000)::text, 2, '0')
        || ':' || lpad((rem / 60000000 % 60)::text, 2, '0')
        || ':' || lpad((rem / 1000000 % 60)::text, 2, '0')
        || '.' || lpad((rem % 1000000)::text, 6, '0') || '+00:00';
END $$;

-- Timestamp text as unix microseconds, by the grammar documented on
-- codec::parse_micros: YYYY-MM-DD, T/t/space, HH:MM:SS (SS up to 60, a leap
-- second), optional fraction (digits past the ninth ignored, rounded half up
-- to the microsecond), optional Z/z or +-HH[[:]MM] up to 23:59. No offset
-- is UTC. The instant must fall in years 0000 to 9999. Anything else is an
-- error.
CREATE OR REPLACE FUNCTION hv_ts_micros(t text) RETURNS bigint
LANGUAGE plpgsql IMMUTABLE STRICT PARALLEL SAFE AS $$
DECLARE
    p text[];
    y bigint;
    mo bigint;
    d bigint;
    h bigint;
    mi bigint;
    s bigint;
    oh bigint := 0;
    om bigint := 0;
    off bigint := 0;
    nanos bigint := 0;
    us bigint;
BEGIN
    p := regexp_match(t,
        '^([0-9]{4})-([0-9]{2})-([0-9]{2})[Tt ]([0-9]{2}):([0-9]{2}):([0-9]{2})([.][0-9]+)?([Zz]|[+-][0-9]{2}(:?[0-9]{2})?)?$');
    IF p IS NOT NULL THEN
        y := p[1]::bigint;
        mo := p[2]::bigint;
        d := p[3]::bigint;
        h := p[4]::bigint;
        mi := p[5]::bigint;
        s := p[6]::bigint;
        IF p[7] IS NOT NULL THEN
            nanos := rpad(left(substr(p[7], 2), 9), 9, '0')::bigint;
        END IF;
        IF p[8] IS NOT NULL AND p[8] NOT IN ('Z', 'z') THEN
            oh := substr(p[8], 2, 2)::bigint;
            om := CASE WHEN length(p[8]) > 3 THEN right(p[8], 2)::bigint ELSE 0 END;
            off := (oh * 3600 + om * 60) * CASE WHEN left(p[8], 1) = '-' THEN -1 ELSE 1 END;
        END IF;
    END IF;
    IF p IS NULL
       OR mo < 1 OR mo > 12 OR d < 1
       -- In parentheses: an IF condition otherwise ends at CASE's first THEN.
       OR d > (CASE
               WHEN mo = 2 THEN
                   CASE WHEN y % 4 = 0 AND (y % 100 <> 0 OR y % 400 = 0) THEN 29 ELSE 28 END
               WHEN mo IN (4, 6, 9, 11) THEN 30
               ELSE 31
               END)
       OR h > 23 OR mi > 59 OR s > 60 OR oh > 23 OR om > 59 THEN
        RAISE EXCEPTION 'hv_ts_micros: cannot read % as a timestamp', quote_literal(left(t, 200))
            USING ERRCODE = 'invalid_datetime_format';
    END IF;
    us := (hv_days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + s - off) * 1000000
        + (nanos + 500) / 1000;
    IF us < -62167219200000000 OR us > 253402300799999999 THEN
        RAISE EXCEPTION 'hv_ts_micros: % is outside years 0000 to 9999', quote_literal(left(t, 200))
            USING ERRCODE = 'datetime_field_overflow';
    END IF;
    RETURN us;
END $$;

-- The wall clock now in unix microseconds, exactly. clock_timestamp(), not
-- now(): it advances within a transaction, as Rust's clock does.
CREATE OR REPLACE FUNCTION hv_now_micros() RETURNS bigint
LANGUAGE sql VOLATILE PARALLEL SAFE AS $$
    SELECT (EXTRACT(EPOCH FROM date_trunc('second', ts)))::bigint * 1000000
         + (EXTRACT(MICROSECONDS FROM ts))::bigint % 1000000
    FROM (SELECT clock_timestamp() AT TIME ZONE 'UTC' AS ts) clock
$$;

-- 0: the interned text; 1: the collection (NULL unless a record URI);
-- 2: the rkey ('' unless a record URI); 3: the kind ('1' record, '0' at://
-- other, '-1' other); anything else NULL. A record URI is at:// and exactly
-- three non-empty '/'-separated parts, whatever they hold (a handle, '?',
-- '#'), as codec::parse_record_uri reads it.
CREATE OR REPLACE FUNCTION hv_ref_part(t text, n bigint) RETURNS text
LANGUAGE plpgsql IMMUTABLE STRICT PARALLEL SAFE AS $$
DECLARE
    rest text;
    parts text[];
BEGIN
    IF left(t, 5) = 'at://' THEN
        rest := substr(t, 6);
        parts := string_to_array(rest, '/');
        IF array_length(parts, 1) = 3 AND parts[1] <> '' AND parts[2] <> '' AND parts[3] <> '' THEN
            RETURN CASE n WHEN 0 THEN parts[1] WHEN 1 THEN parts[2] WHEN 2 THEN parts[3] WHEN 3 THEN '1' END;
        END IF;
        RETURN CASE n WHEN 0 THEN rest WHEN 1 THEN NULL WHEN 2 THEN '' WHEN 3 THEN '0' END;
    END IF;
    RETURN CASE n WHEN 0 THEN t WHEN 1 THEN NULL WHEN 2 THEN '' WHEN 3 THEN '-1' END;
END $$;
