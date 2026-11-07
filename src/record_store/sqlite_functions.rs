//! SQL functions the record store's views and triggers call on SQLite,
//! registered on every connection this process opens.
//!
//! sqlx 0.9's `Any` driver keeps a connection's backend behind a private
//! trait object (`sqlx-core-0.9.0/src/any/connection/mod.rs:27-35`) with no
//! downcast to `SqliteConnection`, so an `after_connect` hook cannot reach
//! the raw handle (`SqliteConnection::lock_handle`,
//! `sqlx-sqlite-0.9.0/src/connection/mod.rs:199`) to register a function.
//! `sqlite3_auto_extension` can: sqlx links one bundled SQLite (3.46.0, from
//! `libsqlite3-sys` 0.30.1, which this crate depends on directly so the two
//! unify), and SQLite runs the entry point for every connection opened after
//! it is registered, from any pool. A connection opened before [`register`],
//! or by another program (the `sqlite3` CLI), has no `hv_*` functions and
//! cannot prepare a query on the compatibility views.
//!
//! Every function is a thin wrapper over `super::codec`, which holds the
//! rules; `migrations/postgres/20261011000000_record_store_functions.sql`
//! implements the same rules for Postgres. The tests below hold the three to
//! agreement. NULL in gives NULL out, for every function but
//! `hv_now_micros`.

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, Once};

use libsqlite3_sys as ffi;

use super::codec;

static REGISTER: Once = Once::new();

/// Register the functions for every SQLite connection opened from now on.
/// Idempotent. Call it before the first pool connects; `db::install_drivers`
/// does.
pub fn register() {
    REGISTER.call_once(|| {
        // SAFETY: `init` has the entry-point signature SQLite expects and
        // lives for the whole process.
        let rc = unsafe { ffi::sqlite3_auto_extension(Some(init)) };
        assert_eq!(
            rc,
            ffi::SQLITE_OK,
            "sqlite3_auto_extension failed with {rc}"
        );
    });
}

type ScalarFn =
    unsafe extern "C" fn(*mut ffi::sqlite3_context, c_int, *mut *mut ffi::sqlite3_value);

/// Pure: the same arguments always give the same result and there are no
/// side effects, so SQLite may call the function from views, triggers and
/// indexes.
const PURE: c_int = ffi::SQLITE_UTF8 | ffi::SQLITE_DETERMINISTIC | ffi::SQLITE_INNOCUOUS;

unsafe extern "C" fn init(
    db: *mut ffi::sqlite3,
    _err: *mut *mut c_char,
    _api: *const ffi::sqlite3_api_routines,
) -> c_int {
    let state = Box::into_raw(Box::new(ConnDicts::default())).cast::<c_void>();
    // SAFETY: as below; SQLite owns `state` from here and frees it with
    // `drop_conn_dicts`, also when registration fails.
    let rc = unsafe {
        ffi::sqlite3_create_function_v2(
            db,
            c"hv_record".as_ptr(),
            2,
            PURE,
            state,
            Some(hv_record),
            None,
            None,
            Some(drop_conn_dicts),
        )
    };
    if rc != ffi::SQLITE_OK {
        return rc;
    }
    let functions: [(&CStr, c_int, c_int, ScalarFn); 7] = [
        (c"hv_cid_text", 1, PURE, hv_cid_text),
        (c"hv_cid_bytes", 1, PURE, hv_cid_bytes),
        (c"hv_ts_text", 1, PURE, hv_ts_text),
        (c"hv_ts_micros", 1, PURE, hv_ts_micros),
        (c"hv_ref_part", 2, PURE, hv_ref_part),
        (
            c"hv_now_micros",
            0,
            ffi::SQLITE_UTF8 | ffi::SQLITE_INNOCUOUS,
            hv_now_micros,
        ),
        // Reads the database, so not deterministic.
        (
            c"hv_collection_id",
            1,
            ffi::SQLITE_UTF8 | ffi::SQLITE_INNOCUOUS,
            hv_collection_id,
        ),
    ];
    for (name, args, flags, function) in functions {
        // SAFETY: `db` is the connection SQLite is initialising; the name is
        // a static C string; no user data or destructor is passed.
        let rc = unsafe {
            ffi::sqlite3_create_function_v2(
                db,
                name.as_ptr(),
                args,
                flags,
                std::ptr::null_mut(),
                Some(function),
                None,
                None,
                None,
            )
        };
        if rc != ffi::SQLITE_OK {
            return rc;
        }
    }
    ffi::SQLITE_OK
}

/// One connection's decoded dictionaries, by `happyview_zstd_dicts.id`.
/// Per connection because a dictionary id means something only in its own
/// database, and a process may have several open.
#[derive(Default)]
struct ConnDicts {
    dicts: Mutex<HashMap<i64, Arc<zstd::dict::DecoderDictionary<'static>>>>,
}

unsafe extern "C" fn drop_conn_dicts(state: *mut c_void) {
    // SAFETY: `state` is the `Box<ConnDicts>` `init` leaked for this
    // connection; SQLite calls this once, when the function is dropped.
    drop(unsafe { Box::from_raw(state.cast::<ConnDicts>()) });
}

/// What a function hands back to SQLite.
enum Out {
    Null,
    Text(String),
    Blob(Vec<u8>),
    Int(i64),
}

/// Why a function failed.
enum Fail {
    Error(String),
    /// SQLite ran out of memory converting an argument.
    NoMem,
}

impl From<String> for Fail {
    fn from(msg: String) -> Self {
        Self::Error(msg)
    }
}

/// Characters of an argument an error message quotes. A message must not
/// copy (and Debug-escape) the whole of a large argument.
const EXCERPT_CHARS: usize = 200;

/// `text` quoted for an error message, cut at [`EXCERPT_CHARS`].
fn excerpt(text: &str) -> String {
    match text.char_indices().nth(EXCERPT_CHARS) {
        Some((end, _)) => format!("{:?}…", &text[..end]),
        None => format!("{text:?}"),
    }
}

/// Run `f` and hand its result, or its error, to SQLite. A panic must not
/// unwind into C, so it becomes an SQL error too.
unsafe fn finish(ctx: *mut ffi::sqlite3_context, f: impl FnOnce() -> Result<Out, Fail>) {
    let out = catch_unwind(AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(Fail::Error("hv function panicked".into())));
    // SAFETY: `ctx` is the live context SQLite passed to the function; every
    // buffer is copied by SQLite (`SQLITE_TRANSIENT`) before it is dropped.
    unsafe {
        match out {
            Ok(Out::Null) => ffi::sqlite3_result_null(ctx),
            Ok(Out::Text(s)) => ffi::sqlite3_result_text64(
                ctx,
                s.as_ptr().cast(),
                s.len() as u64,
                ffi::SQLITE_TRANSIENT(),
                ffi::SQLITE_UTF8 as u8,
            ),
            Ok(Out::Blob(b)) if b.is_empty() => ffi::sqlite3_result_zeroblob(ctx, 0),
            Ok(Out::Blob(b)) => ffi::sqlite3_result_blob64(
                ctx,
                b.as_ptr().cast(),
                b.len() as u64,
                ffi::SQLITE_TRANSIENT(),
            ),
            Ok(Out::Int(i)) => ffi::sqlite3_result_int64(ctx, i),
            Err(Fail::NoMem) => ffi::sqlite3_result_error_nomem(ctx),
            // The length is clamped, never wrapped: a negative length would
            // have SQLite read on to a NUL the `String` does not have.
            Err(Fail::Error(msg)) => ffi::sqlite3_result_error(
                ctx,
                msg.as_ptr().cast(),
                c_int::try_from(msg.len()).unwrap_or(c_int::MAX),
            ),
        }
    }
}

/// Argument `i`, or `None` when it is SQL NULL.
unsafe fn arg(argv: *mut *mut ffi::sqlite3_value, i: usize) -> Option<*mut ffi::sqlite3_value> {
    // SAFETY: SQLite passes as many values as the function was registered with.
    let value = unsafe { *argv.add(i) };
    (unsafe { ffi::sqlite3_value_type(value) } != ffi::SQLITE_NULL).then_some(value)
}

/// The `len` bytes at `ptr`, borrowed until the function returns. SQLite
/// hands back no pointer for an empty text or blob, and otherwise only when
/// it ran out of memory converting the value.
unsafe fn borrowed<'a>(
    value: *mut ffi::sqlite3_value,
    ptr: *const u8,
    len: usize,
) -> Result<&'a [u8], Fail> {
    if !ptr.is_null() {
        // SAFETY: SQLite keeps the buffer valid until the function returns.
        return Ok(unsafe { std::slice::from_raw_parts(ptr, len) });
    }
    // SAFETY: `value` is a live argument.
    let kind = unsafe { ffi::sqlite3_value_type(value) };
    if len == 0 && matches!(kind, ffi::SQLITE_TEXT | ffi::SQLITE_BLOB) {
        Ok(&[])
    } else {
        Err(Fail::NoMem)
    }
}

/// The value's bytes.
unsafe fn blob<'a>(value: *mut ffi::sqlite3_value) -> Result<&'a [u8], Fail> {
    // SAFETY: `bytes` is read after `blob`, as SQLite requires.
    unsafe {
        let ptr = ffi::sqlite3_value_blob(value) as *const u8;
        let len = ffi::sqlite3_value_bytes(value) as usize;
        borrowed(value, ptr, len)
    }
}

/// The value as UTF-8 text.
unsafe fn text<'a>(value: *mut ffi::sqlite3_value) -> Result<&'a str, Fail> {
    // SAFETY: as for `blob`, with `text` read before `bytes`.
    let bytes = unsafe {
        let ptr = ffi::sqlite3_value_text(value);
        let len = ffi::sqlite3_value_bytes(value) as usize;
        borrowed(value, ptr, len)?
    };
    std::str::from_utf8(bytes).map_err(|e| Fail::Error(e.to_string()))
}

/// The bytes of dictionary `id`, read on the connection running the query.
unsafe fn load_dict(db: *mut ffi::sqlite3, id: i64) -> Result<Vec<u8>, Fail> {
    // SAFETY: `db` is the live connection from `sqlite3_context_db_handle`;
    // a scalar function may run its own statements on it. The statement is
    // finalized on every path.
    unsafe {
        let mut stmt = std::ptr::null_mut();
        let rc = ffi::sqlite3_prepare_v2(
            db,
            c"SELECT dict FROM happyview_zstd_dicts WHERE id = ?1".as_ptr(),
            -1,
            &mut stmt,
            std::ptr::null_mut(),
        );
        if rc != ffi::SQLITE_OK {
            return Err(format!("hv_record: cannot read zstd dictionary {id} (error {rc})").into());
        }
        ffi::sqlite3_bind_int64(stmt, 1, id);
        let out = match ffi::sqlite3_step(stmt) {
            ffi::SQLITE_ROW => {
                let ptr = ffi::sqlite3_column_blob(stmt, 0) as *const u8;
                let len = ffi::sqlite3_column_bytes(stmt, 0) as usize;
                if !ptr.is_null() {
                    Ok(std::slice::from_raw_parts(ptr, len).to_vec())
                } else if len == 0 {
                    Ok(Vec::new())
                } else {
                    Err(Fail::NoMem)
                }
            }
            ffi::SQLITE_DONE => {
                Err(format!("hv_record: zstd dictionary {id} does not exist").into())
            }
            rc => Err(format!("hv_record: cannot read zstd dictionary {id} (error {rc})").into()),
        };
        ffi::sqlite3_finalize(stmt);
        out
    }
}

/// The id of collection `nsid`, read on the connection running the query.
unsafe fn collection_id(db: *mut ffi::sqlite3, nsid: &str) -> Result<Option<i64>, Fail> {
    // SAFETY: as for `load_dict`; the bound text is copied by SQLite
    // (`SQLITE_TRANSIENT`), and the statement is finalized on every path.
    unsafe {
        let mut stmt = std::ptr::null_mut();
        let rc = ffi::sqlite3_prepare_v2(
            db,
            c"SELECT id FROM happyview_collections WHERE nsid = ?1".as_ptr(),
            -1,
            &mut stmt,
            std::ptr::null_mut(),
        );
        if rc != ffi::SQLITE_OK {
            return Err(
                format!("hv_collection_id: cannot read the collections (error {rc})").into(),
            );
        }
        let Ok(len) = c_int::try_from(nsid.len()) else {
            ffi::sqlite3_finalize(stmt);
            return Err(Fail::NoMem);
        };
        ffi::sqlite3_bind_text(
            stmt,
            1,
            nsid.as_ptr().cast::<c_char>(),
            len,
            ffi::SQLITE_TRANSIENT(),
        );
        let out = match ffi::sqlite3_step(stmt) {
            ffi::SQLITE_ROW => Ok(Some(ffi::sqlite3_column_int64(stmt, 0))),
            ffi::SQLITE_DONE => Ok(None),
            rc => Err(format!("hv_collection_id: cannot read the collections (error {rc})").into()),
        };
        ffi::sqlite3_finalize(stmt);
        out
    }
}

/// `hv_collection_id(nsid)`: the collection's id, or NULL when the database
/// has no such collection. The Postgres function of the same name answers
/// the same.
unsafe extern "C" fn hv_collection_id(
    ctx: *mut ffi::sqlite3_context,
    _argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    unsafe {
        let value = arg(argv, 0).map(|v| text(v));
        let db = ffi::sqlite3_context_db_handle(ctx);
        finish(ctx, || match value {
            None => Ok(Out::Null),
            Some(nsid) => Ok(collection_id(db, nsid?)?.map_or(Out::Null, Out::Int)),
        })
    }
}

/// `hv_record(body, body_dict)`: the stored body as JSON text. A NULL or `0`
/// dictionary means the body is stored raw; a positive one is the
/// `happyview_zstd_dicts` row it was compressed with, read once per
/// connection. A negative id is no dictionary at all, so it is an error.
unsafe extern "C" fn hv_record(
    ctx: *mut ffi::sqlite3_context,
    _argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    unsafe {
        let Some(body) = arg(argv, 0) else {
            return finish(ctx, || Ok(Out::Null));
        };
        let body = blob(body);
        let dict = arg(argv, 1)
            .map(|v| ffi::sqlite3_value_int64(v))
            .unwrap_or(0);
        let state = ffi::sqlite3_user_data(ctx).cast::<ConnDicts>().cast_const();
        let db = ffi::sqlite3_context_db_handle(ctx);
        finish(ctx, || {
            let body = body?;
            match dict {
                0 => {
                    return std::str::from_utf8(body)
                        .map(|s| Out::Text(s.to_string()))
                        .map_err(|e| format!("hv_record: stored body is not UTF-8: {e}").into());
                }
                ..0 => return Err(format!("hv_record: {dict} is not a dictionary id").into()),
                _ => {}
            }
            // SAFETY: `state` is this connection's `ConnDicts`, alive until
            // SQLite drops the function, which outlives every call.
            let dicts = &(*state).dicts;
            let cached = dicts
                .lock()
                .expect("dictionary cache poisoned")
                .get(&dict)
                .cloned();
            let decoder = match cached {
                Some(decoder) => decoder,
                None => {
                    let bytes = load_dict(db, dict)?;
                    let decoder = Arc::new(zstd::dict::DecoderDictionary::copy(&bytes));
                    dicts
                        .lock()
                        .expect("dictionary cache poisoned")
                        .insert(dict, Arc::clone(&decoder));
                    decoder
                }
            };
            super::compression::decompress(body, &decoder)
                .map(Out::Text)
                .map_err(|e| format!("hv_record: dictionary {dict}: {e}").into())
        })
    }
}

unsafe extern "C" fn hv_cid_text(
    ctx: *mut ffi::sqlite3_context,
    _argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    unsafe {
        let value = arg(argv, 0).map(|v| blob(v));
        finish(ctx, || match value {
            None => Ok(Out::Null),
            Some(b) => Ok(Out::Text(codec::decode_cid(b?))),
        })
    }
}

unsafe extern "C" fn hv_cid_bytes(
    ctx: *mut ffi::sqlite3_context,
    _argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    unsafe {
        let value = arg(argv, 0).map(|v| text(v));
        finish(ctx, || match value {
            None => Ok(Out::Null),
            Some(t) => Ok(Out::Blob(codec::encode_cid(t?))),
        })
    }
}

unsafe extern "C" fn hv_ts_text(
    ctx: *mut ffi::sqlite3_context,
    _argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    unsafe {
        let value = arg(argv, 0).map(|v| ffi::sqlite3_value_int64(v));
        finish(ctx, || {
            Ok(value.map_or(Out::Null, |us| Out::Text(codec::render_micros(us))))
        })
    }
}

unsafe extern "C" fn hv_ts_micros(
    ctx: *mut ffi::sqlite3_context,
    _argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    unsafe {
        let value = arg(argv, 0).map(|v| text(v));
        finish(ctx, || match value {
            None => Ok(Out::Null),
            Some(t) => {
                let t = t?;
                codec::parse_micros(t).map(Out::Int).ok_or_else(|| {
                    format!("hv_ts_micros: cannot read {} as a timestamp", excerpt(t)).into()
                })
            }
        })
    }
}

unsafe extern "C" fn hv_ref_part(
    ctx: *mut ffi::sqlite3_context,
    _argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    unsafe {
        let target = arg(argv, 0).map(|v| text(v));
        let part = arg(argv, 1).map(|v| ffi::sqlite3_value_int64(v));
        finish(ctx, || match (target, part) {
            (Some(t), Some(n)) => Ok(codec::ref_part(t?, n).map_or(Out::Null, Out::Text)),
            _ => Ok(Out::Null),
        })
    }
}

unsafe extern "C" fn hv_now_micros(
    ctx: *mut ffi::sqlite3_context,
    _argc: c_int,
    _argv: *mut *mut ffi::sqlite3_value,
) {
    unsafe { finish(ctx, || Ok(Out::Int(codec::now_micros()))) }
}

#[cfg(test)]
mod tests {
    use sqlx::pool::PoolConnection;

    use crate::db::{DatabaseBackend, adapt_sql};
    use crate::record_store::codec;
    use crate::test_support::test_pools;

    const POST_CID: &str = "bafyreie5737gdxlw5i64vzichcalba3z2v5n6icifvx5xytvske7mr3hpm";

    /// A small deterministic generator, so a failing case reproduces.
    struct Xorshift(u64);
    impl Xorshift {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
    }

    /// One connection per backend. On Postgres its session time zone is not
    /// UTC and not a whole hour, so a function that leaned on the session
    /// time zone would disagree with Rust.
    async fn connections() -> Vec<(PoolConnection<sqlx::Any>, DatabaseBackend)> {
        let mut out = Vec::new();
        for (pool, backend) in test_pools().await {
            let mut conn = pool.acquire().await.expect("acquire");
            if backend == DatabaseBackend::Postgres {
                crate::db::query("SET TIME ZONE 'America/St_Johns'")
                    .execute(&mut *conn)
                    .await
                    .expect("set the session time zone");
            }
            out.push((conn, backend));
        }
        out
    }

    fn cids() -> Vec<String> {
        let mut tweaked = POST_CID.to_string();
        tweaked.pop();
        tweaked.push('n'); // non-zero trailing bits
        let mut cids: Vec<String> = [
            POST_CID,
            "",
            "c50",
            "c1",
            "b",
            "ba",
            "bae", // canonical base32 of the single byte 0x01
            "baa", // canonical base32 of 0x00: not a CIDv1
            "bafy",
            "bafyBAD",
            "zafy",
            "f01711220",
            "B",
            "é",
            "QmYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbdG",
            &POST_CID.to_uppercase(),
            &format!("B{}", &POST_CID[1..]),
            &tweaked,
            &format!("{POST_CID}="),
            &format!("{POST_CID}a"),
            &POST_CID[..POST_CID.len() - 1],
            &POST_CID[..POST_CID.len() - 2],
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let mut rng = Xorshift(0x0c1d_5eed_0000_0001);
        for _ in 0..100 {
            let len = (rng.next() % 40) as usize + 1;
            let mut bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            bytes[0] = 0x01;
            cids.push(format!("b{}", codec::base32_encode(&bytes)));
            let garbage: String = (0..len)
                .map(|_| char::from_u32((rng.next() % 0x2FF) as u32 + 0x20).unwrap_or('x'))
                .collect();
            cids.push(garbage);
        }
        cids
    }

    #[tokio::test]
    async fn the_sql_cid_codec_agrees_with_rust() {
        for (mut conn, backend) in connections().await {
            let sql = adapt_sql(
                "SELECT hv_cid_bytes(?), hv_cid_text(hv_cid_bytes(?))",
                backend,
            );
            for cid in cids() {
                let (bytes, text): (Vec<u8>, String) = crate::db::query_as(&sql)
                    .bind(&cid)
                    .bind(&cid)
                    .fetch_one(&mut *conn)
                    .await
                    .unwrap_or_else(|e| panic!("{backend:?} {cid:?}: {e}"));
                assert_eq!(bytes, codec::encode_cid(&cid), "{backend:?} {cid:?}");
                assert_eq!(text, cid, "{backend:?}");
            }
        }
    }

    /// `hv_cid_text` on bytes no `hv_cid_bytes` call produced: any binary
    /// form, and text behind the marker.
    #[tokio::test]
    async fn the_sql_cid_decoder_agrees_with_rust_on_any_bytes() {
        let mut rng = Xorshift(0xb17e_5eed_0000_0002);
        let mut inputs: Vec<Vec<u8>> = vec![
            vec![],
            vec![0x00],
            vec![0x01],
            vec![0xff],
            vec![0x00, b'c', b'1'],
        ];
        for _ in 0..100 {
            let len = (rng.next() % 50) as usize + 1;
            let mut bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            if bytes[0] == 0 {
                bytes[0] = 1;
            }
            inputs.push(bytes);
            let text: String = (0..len)
                .map(|_| char::from_u32((rng.next() % 0x2FF) as u32 + 0x20).unwrap_or('x'))
                .collect();
            let mut marked = vec![0x00];
            marked.extend_from_slice(text.as_bytes());
            inputs.push(marked);
        }
        for (mut conn, backend) in connections().await {
            let sql = adapt_sql("SELECT hv_cid_text(?)", backend);
            for bytes in &inputs {
                let (text,): (String,) = crate::db::query_as(&sql)
                    .bind(bytes.clone())
                    .fetch_one(&mut *conn)
                    .await
                    .unwrap_or_else(|e| panic!("{backend:?} {bytes:?}: {e}"));
                assert_eq!(text, codec::decode_cid(bytes), "{backend:?} {bytes:?}");
            }
        }
    }

    /// Text `parse_micros` reads. Each comment says what the case pins.
    const TIMESTAMPS: &[&str] = &[
        "2026-10-11T12:34:56.123456789+00:00", // db::now_rfc3339(): 9 digits, rounded
        "2026-03-16 12:34:56",                 // SQLite datetime('now'): no offset is UTC
        "2026-03-18 12:00:00.123456+00",       // Postgres timestamptz::text
        "2026-01-01T00:00:00Z",
        "1969-12-31T23:59:59.5+00:00",
        "2026-10-11T08:34:56.5-04:00",
        "2026-10-11T12:34:56",  // no offset, T separator
        "2026-10-11t12:34:56z", // lowercase t and z
        "2026-10-11t12:34:56.5",
        "2026-10-11 12:34:56Z",
        "2026-10-11T18:04:56.5+0530",
        "2026-10-11T18:04:56.5+05:30",
        "2026-10-11 18:04:56.5+05:30",
        "2026-10-11T08:34:56-04",
        "2026-10-11 08:34:56.5-04",
        "2026-10-11T12:34:56-00:00",
        "2026-10-11T12:34:56.0000005Z",    // a .5 µs tie rounds up
        "2026-10-11T12:34:56.0000004999Z", // digits past the ninth are ignored
        "2026-10-11T12:34:56.00000049999999999999Z",
        "2026-10-11T12:34:56.9999995Z", // rounds into the next second
        "1969-12-31T23:59:59.0000005Z", // pre-epoch tie: up, towards zero
        "1969-12-31T23:59:59.0000015Z",
        "1969-12-31T23:59:59.9999995Z", // pre-epoch, rounds to the epoch
        "1969-12-31T23:59:59.9999994999Z",
        "1969-12-31T23:59:59.000000Z",
        "1900-02-28T23:59:59.9999995Z",
        "1583-01-01T00:00:00Z",
        "1000-06-15 12:00:00",
        "2016-12-31T23:59:60Z", // a leap second reads as the next second
        "2016-12-31T23:59:60.5Z",
        "2026-10-11T12:34:60+05:30",
        "2024-02-29T00:00:00Z",
        "2000-02-29 00:00:00",
        "0000-01-01T00:00:00Z",        // the first instant
        "0000-02-29T00:00:00Z",        // year 0 is a leap year
        "0001-01-01T00:00:00+23:59",   // into year 0 by its offset
        "9999-12-31T23:59:59.999999Z", // the last instant
        "9999-12-31T23:59:59.9999994Z",
        "2026-10-11T12:34:56.1+23:59",
        "2026-10-11T12:34:56-23:59",
    ];

    /// Text `parse_micros` refuses; the SQL function must raise an error.
    const NOT_TIMESTAMPS: &[&str] = &[
        "",
        "not a timestamp",
        "yesterday",
        "now",
        "today",
        "epoch",
        "infinity",
        "-infinity",
        "2026-10-11",
        "2026-10-11T12:34",
        "12:34:56",
        " 2026-10-11 12:34:56",
        "2026-10-11 12:34:56 ",
        "2026-10-11T12:34:56Z\n",
        "2026-02-29T00:00:00Z",
        "1900-02-29 00:00:00",
        "2026-04-31 00:00:00",
        "2026-13-01 00:00:00",
        "2026-00-10 00:00:00",
        "2026-10-00 00:00:00",
        "2026-10-11T24:00:00Z",
        "2026-10-11T12:60:00Z",
        "2026-10-11T12:34:61Z",
        "2026-10-11T12:34:56+24:00",
        "2026-10-11T12:34:56+05:60",
        "2026-10-11T12:34:56+5",
        "2026-10-11T12:34:56+05:3",
        "2026-10-11T12:34:56+053",
        "2026-10-11T12:34:56+05:30:00",
        "2026-10-11T12:34:56.Z",
        "2026-10-11T12:34:56.",
        "2026-10-11T12:34:56,5Z",
        "2026-1-11 12:34:56",
        "2026-10-11 1:34:56",
        "+2026-10-11T12:34:56Z",
        "12026-10-11T12:34:56Z",
        "2026/10/11 12:34:56",
        "2026-10-11T12:34:56ZZ",
        "2026-10-11T12:34:56 +00:00",
        "2026-10-11T12:34:56 UTC",
        "2026-10-11T12:34:56+00:00 BC",
        "2026-10-11T12:34:56 America/New_York",
        "9999-12-31T23:59:59.9999995Z", // rounds past the last instant
        "0000-01-01T00:00:00+00:01",    // before the first
        "9999-12-31T23:59:59-00:01",    // after the last
        "２０２６-10-11T12:34:56Z",
        "2026-10-11T12:34:56.５Z",
        "2026-10-11Ｔ12:34:56Z",
        "٢٠٢٦-10-11T12:34:56Z",
    ];

    /// Random text in every shape the grammar allows, valid or not.
    fn random_timestamps() -> Vec<String> {
        let mut rng = Xorshift(0x7135_5eed_0000_0003);
        let mut out = Vec::new();
        for _ in 0..300 {
            let mut pick = |n: u64| rng.next() % n;
            let year = pick(10_000);
            let month = pick(14);
            let day = pick(33);
            let hour = pick(26);
            let minute = pick(61);
            let second = pick(62);
            let separator = ["T", "t", " "][pick(3) as usize];
            let fraction = match pick(4) {
                0 => String::new(),
                _ => {
                    let digits = pick(12) + 1;
                    let mut f = String::from(".");
                    for _ in 0..digits {
                        f.push(char::from(b'0' + pick(10) as u8));
                    }
                    f
                }
            };
            let offset = match pick(6) {
                0 => String::new(),
                1 => ["Z", "z"][pick(2) as usize].to_string(),
                n => {
                    let sign = ["+", "-"][pick(2) as usize];
                    let (h, m) = (pick(25), pick(61));
                    match n {
                        2 => format!("{sign}{h:02}"),
                        3 => format!("{sign}{h:02}{m:02}"),
                        _ => format!("{sign}{h:02}:{m:02}"),
                    }
                }
            };
            out.push(format!(
                "{year:04}-{month:02}-{day:02}{separator}{hour:02}:{minute:02}:{second:02}{fraction}{offset}"
            ));
        }
        out
    }

    #[tokio::test]
    async fn the_sql_timestamp_parser_agrees_with_rust() {
        for text in TIMESTAMPS {
            assert!(codec::parse_micros(text).is_some(), "rust reads {text:?}");
        }
        for text in NOT_TIMESTAMPS {
            assert_eq!(codec::parse_micros(text), None, "rust refuses {text:?}");
        }
        let inputs: Vec<String> = TIMESTAMPS
            .iter()
            .chain(NOT_TIMESTAMPS)
            .map(|s| s.to_string())
            .chain(random_timestamps())
            .collect();
        let mut accepted = 0;
        for (mut conn, backend) in connections().await {
            let sql = adapt_sql(
                "SELECT hv_ts_micros(?), hv_ts_text(hv_ts_micros(?))",
                backend,
            );
            for text in &inputs {
                let got: Result<(i64, String), _> = crate::db::query_as(&sql)
                    .bind(text)
                    .bind(text)
                    .fetch_one(&mut *conn)
                    .await;
                match (codec::parse_micros(text), got) {
                    (Some(us), Ok((micros, rendered))) => {
                        assert_eq!(micros, us, "{backend:?} {text:?}");
                        assert_eq!(rendered, codec::render_micros(us), "{backend:?} {text:?}");
                        accepted += 1;
                    }
                    (None, Err(_)) => {}
                    (expected, got) => {
                        panic!("{backend:?} {text:?}: rust {expected:?}, sql {got:?}")
                    }
                }
            }
            let null: (Option<i64>,) = crate::db::query_as("SELECT hv_ts_micros(NULL)")
                .fetch_one(&mut *conn)
                .await
                .expect("null in, null out");
            assert_eq!(null.0, None);
        }
        assert!(
            accepted > TIMESTAMPS.len(),
            "the random cases include valid ones"
        );
    }

    #[tokio::test]
    async fn the_sql_timestamp_renderer_agrees_with_rust() {
        let mut values = vec![
            0,
            -1,
            1,
            -500_000,
            999_999,
            -1_000_000,
            codec::MIN_MICROS,
            codec::MAX_MICROS,
            codec::MIN_MICROS - 1,
            codec::MAX_MICROS + 1,
            i64::MIN,
            i64::MAX,
            codec::now_micros(),
        ];
        let mut rng = Xorshift(0x7e4d_5eed_0000_0004);
        let span = (codec::MAX_MICROS - codec::MIN_MICROS) as u64;
        for _ in 0..300 {
            values.push(codec::MIN_MICROS + (rng.next() % span) as i64);
        }
        for (mut conn, backend) in connections().await {
            let sql = adapt_sql("SELECT hv_ts_text(?)", backend);
            for us in &values {
                let (text,): (String,) = crate::db::query_as(&sql)
                    .bind(*us)
                    .fetch_one(&mut *conn)
                    .await
                    .unwrap_or_else(|e| panic!("{backend:?} {us}: {e}"));
                assert_eq!(text, codec::render_micros(*us), "{backend:?} {us}");
            }
        }
    }

    /// An error quotes at most a slice of its argument, however large.
    #[tokio::test]
    async fn an_error_quotes_only_an_excerpt_of_a_huge_argument() {
        let huge = "\u{1}".repeat(1_000_000);
        for (mut conn, backend) in connections().await {
            let err = crate::db::query(&adapt_sql("SELECT hv_ts_micros(?)", backend))
                .bind(&huge)
                .execute(&mut *conn)
                .await
                .expect_err("not a timestamp");
            let msg = err.to_string();
            assert!(msg.contains("hv_ts_micros"), "{backend:?}: {msg}");
            assert!(msg.len() < 2_000, "{backend:?}: {} bytes", msg.len());
        }
    }

    const TARGETS: &[&str] = &[
        "at://did:plc:a/app.bsky.feed.post/3l",
        "at://did:plc:a",
        "at://did:plc:a/",
        "at://did:plc:a/c",
        "at://did:plc:a/c/",
        "at://did:plc:a//r",
        "at://alice.example.com/app.bsky.feed.post/3l", // a handle authority
        "at://alice.example.com/app.bsky.feed.post/3l/extra",
        "at://did:plc:a/c/r?x=1",
        "at://did:plc:a/c/r#f",
        "at://did:plc:a/c?x=/r",
        "at://did:plc:a/c#/r",
        "at://did:plc:a/c/r\n",
        "at://did:plc:a/c/r/",
        "at://did:plc:a/c/r/x/y",
        "at://",
        "at:///c/r",
        "AT://did:plc:a/c/r",
        "at:/did:plc:a/c/r",
        "at://did:plc:é/c/ré",
        "https://example.com",
        "",
    ];

    #[tokio::test]
    async fn the_sql_ref_parts_agree_with_rust() {
        for (mut conn, backend) in connections().await {
            let sql = adapt_sql("SELECT hv_ref_part(?, ?)", backend);
            for target in TARGETS {
                for part in 0..5 {
                    let (got,): (Option<String>,) = crate::db::query_as(&sql)
                        .bind(*target)
                        .bind(part)
                        .fetch_one(&mut *conn)
                        .await
                        .unwrap_or_else(|e| panic!("{backend:?} {target:?} {part}: {e}"));
                    assert_eq!(
                        got,
                        codec::ref_part(target, part),
                        "{backend:?} {target:?} part {part}"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn every_function_maps_null_to_null() {
        for (mut conn, backend) in connections().await {
            let texts: (
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
            ) = crate::db::query_as(
                "SELECT hv_cid_text(NULL), hv_ts_text(NULL), \
                     hv_ref_part(NULL, 0), hv_ref_part('x', NULL)",
            )
            .fetch_one(&mut *conn)
            .await
            .unwrap_or_else(|e| panic!("{backend:?}: {e}"));
            assert_eq!(texts, (None, None, None, None), "{backend:?}");
            let others: (Option<Vec<u8>>, Option<i64>) =
                crate::db::query_as("SELECT hv_cid_bytes(NULL), hv_ts_micros(NULL)")
                    .fetch_one(&mut *conn)
                    .await
                    .unwrap_or_else(|e| panic!("{backend:?}: {e}"));
            assert_eq!(others, (None, None), "{backend:?}");

            let before = codec::now_micros();
            let (now,): (i64,) = crate::db::query_as("SELECT hv_now_micros()")
                .fetch_one(&mut *conn)
                .await
                .unwrap_or_else(|e| panic!("{backend:?}: {e}"));
            let after = codec::now_micros();
            // The database's clock, when it is another host, may drift a little.
            assert!(
                (before - 5_000_000..=after + 5_000_000).contains(&now),
                "{backend:?}: {before} <= {now} <= {after}"
            );
        }
    }

    /// SQLite hands back no pointer for an empty blob; that is an empty
    /// argument, not an out-of-memory error.
    #[tokio::test]
    async fn empty_arguments_are_empty_not_errors() {
        let pool = crate::test_support::memory_pool().await;
        let (text, bytes, record): (String, Vec<u8>, String) =
            crate::db::query_as("SELECT hv_cid_text(x''), hv_cid_bytes(x''), hv_record(x'', NULL)")
                .fetch_one(&pool)
                .await
                .expect("empty blobs");
        assert_eq!(
            (text.as_str(), bytes.as_slice(), record.as_str()),
            ("", &[][..], "")
        );
        let (text, bytes): (String, Vec<u8>) =
            crate::db::query_as("SELECT hv_cid_text(''), hv_cid_bytes('')")
                .fetch_one(&pool)
                .await
                .expect("empty text");
        assert_eq!((text.as_str(), bytes.as_slice()), ("", &[][..]));
    }

    #[tokio::test]
    async fn sqlite_reads_raw_bodies_and_refuses_an_unknown_dictionary() {
        let pool = crate::test_support::memory_pool().await;
        let (text,): (String,) =
            crate::db::query_as("SELECT hv_record(CAST('{\"via\":\"x\"}' AS BLOB), NULL)")
                .fetch_one(&pool)
                .await
                .expect("raw body");
        assert_eq!(text, r#"{"via":"x"}"#);
        let (zero,): (String,) = crate::db::query_as("SELECT hv_record(CAST('{}' AS BLOB), 0)")
            .fetch_one(&pool)
            .await
            .expect("raw body, compression considered");
        assert_eq!(zero, "{}");
        let (null,): (Option<String>,) = crate::db::query_as("SELECT hv_record(NULL, 7)")
            .fetch_one(&pool)
            .await
            .expect("null in, null out");
        assert_eq!(null, None);
        assert!(
            crate::db::query("SELECT hv_record(x'00', 7)")
                .execute(&pool)
                .await
                .is_err()
        );
        assert!(
            crate::db::query("SELECT hv_record(CAST('{}' AS BLOB), -1)")
                .execute(&pool)
                .await
                .is_err(),
            "a negative dictionary id is an error, not a raw body"
        );
        assert!(
            crate::db::query("SELECT hv_record(x'ff', NULL)")
                .execute(&pool)
                .await
                .is_err(),
            "a body that is not UTF-8 is an error"
        );
    }

    /// The functions reach a pool built straight from `PoolOptions`, without
    /// `db::connect`, because `sqlite3_auto_extension` covers every
    /// connection the process opens after `register`.
    #[tokio::test]
    async fn a_pool_opened_without_db_connect_has_the_functions() {
        crate::db::install_drivers();
        let path = std::env::temp_dir().join(format!("hv-fns-{}.db", uuid::Uuid::new_v4()));
        let pool = sqlx::pool::PoolOptions::<sqlx::Any>::new()
            .max_connections(3)
            .connect(&format!("sqlite://{}?mode=rwc", path.display()))
            .await
            .expect("connect");
        let mut conns = Vec::new();
        for _ in 0..3 {
            conns.push(pool.acquire().await.expect("acquire"));
        }
        for conn in &mut conns {
            let (text,): (String,) = crate::db::query_as("SELECT hv_ts_text(0)")
                .fetch_one(&mut **conn)
                .await
                .expect("function on every connection");
            assert_eq!(text, "1970-01-01T00:00:00.000000+00:00");
        }
        drop(conns);
        pool.close().await;
        let _ = std::fs::remove_file(&path);
    }

    /// Calling `register` again changes nothing.
    #[tokio::test]
    async fn register_is_idempotent() {
        super::register();
        super::register();
        let pool = crate::test_support::memory_pool().await;
        let (text,): (String,) = crate::db::query_as("SELECT hv_cid_text(x'00633530')")
            .fetch_one(&pool)
            .await
            .expect("still registered");
        assert_eq!(text, "c50");
    }
}
