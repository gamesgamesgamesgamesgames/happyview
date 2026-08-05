use std::path::{Path, PathBuf};

/// Sizes and free space relevant to a VACUUM.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DiskReport {
    pub db_bytes: u64,
    pub wal_bytes: u64,
    /// `None` means the free space on the database's filesystem could not be
    /// measured (not that it measured zero).
    pub db_fs_free: Option<u64>,
    /// `None` means the free space on the temp filesystem could not be
    /// measured (not that it measured zero).
    pub temp_fs_free: Option<u64>,
    /// Whether the database and temp directories resolve to the same
    /// filesystem (compared by device id), which changes how much headroom a
    /// VACUUM needs — see `feasibility`.
    pub same_filesystem: bool,
    pub db_path: String,
    pub temp_path: String,
}

/// Whether a VACUUM has room to run.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum VacuumFeasibility {
    Ok,
    Insufficient {
        needed: u64,
        available: u64,
        path: String,
    },
    /// Free space on `path` could not be measured at all. Treated as blocking
    /// — a vacuum is not run on a number we don't have — but reported
    /// distinctly from `Insufficient` so an operator isn't sent chasing disk
    /// space that was never actually the problem.
    Unknown {
        path: String,
    },
}

/// VACUUM rebuilds the whole database, so it needs the database's size again
/// plus a margin. `db_bytes` must already include the WAL — a stranded WAL is
/// exactly the case this feature targets, and the temp copy plus the rebuilt
/// WAL both have to fit alongside it, not just alongside the main file. Both
/// the database's filesystem and the temp directory's are checked, because
/// SQLite writes the rebuild's temporary file to the temp directory, which is
/// frequently a different filesystem inside a container.
///
/// The margin depends on whether the two directories share a filesystem:
/// - Different filesystems: each needs the database's size again plus 20%,
///   checked independently.
/// - Same filesystem: that one filesystem must hold the temp copy *and*
///   absorb the whole rebuild through the WAL, so the margin doubles to 120%
///   (2.2x the database size in total).
pub fn feasibility(
    db_bytes: u64,
    db_fs_free: Option<u64>,
    temp_fs_free: Option<u64>,
    db_path: &str,
    temp_path: &str,
    same_filesystem: bool,
) -> VacuumFeasibility {
    let needed = if same_filesystem {
        db_bytes + (db_bytes * 12) / 10
    } else {
        db_bytes + db_bytes / 5
    };
    if needed == 0 {
        return VacuumFeasibility::Ok;
    }

    match (db_fs_free, temp_fs_free) {
        (None, _) => VacuumFeasibility::Unknown {
            path: db_path.to_string(),
        },
        (_, None) => VacuumFeasibility::Unknown {
            path: temp_path.to_string(),
        },
        (Some(db_free), Some(temp_free)) => {
            // Report whichever filesystem has least room, so the message
            // names the one that will actually fail.
            let (available, path) = if db_free <= temp_free {
                (db_free, db_path)
            } else {
                (temp_free, temp_path)
            };
            if available >= needed {
                VacuumFeasibility::Ok
            } else {
                VacuumFeasibility::Insufficient {
                    needed,
                    available,
                    path: path.to_string(),
                }
            }
        }
    }
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Free space at `path`, or `None` if it could not be measured (e.g. the
/// directory doesn't exist or isn't readable) — distinct from a real zero.
fn free_space(path: &Path) -> Option<u64> {
    fs4::available_space(path).ok()
}

/// The directory containing `path`, falling back to `.` when `path` has no
/// parent component. `Path::parent()` returns `Some("")` for a bare filename
/// like `happyview.db`, not `None`, so the empty case must be checked
/// explicitly — the same guard `db::connect` uses when creating the data
/// directory.
fn containing_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

/// The WAL file's path for a given database path. SQLite names it by
/// appending `-wal` to the *entire* main file name, not by replacing its
/// extension — `/data/happyview.db` becomes `/data/happyview.db-wal`, and the
/// extensionless `/data/hv` becomes `/data/hv-wal`.
fn wal_path_for(db_path: &Path) -> PathBuf {
    let mut name = db_path.as_os_str().to_os_string();
    name.push("-wal");
    PathBuf::from(name)
}

/// Whether `a` and `b` live on the same filesystem, compared by device id.
/// Conservatively `false` if either path's metadata can't be read.
fn same_filesystem(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(ma), Ok(mb)) => ma.dev() == mb.dev(),
        _ => false,
    }
}

/// The directory SQLite will use for VACUUM's temporary database.
fn temp_dir() -> PathBuf {
    std::env::var_os("SQLITE_TMPDIR")
        .or_else(|| std::env::var_os("TMPDIR"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// Measure database and WAL sizes plus free space. Returns `None` when the URL
/// is not a file-backed SQLite database.
pub fn report(db_url: &str) -> Option<DiskReport> {
    let db_path = crate::db::sqlite_path_from_url(db_url)?;
    let wal_path = wal_path_for(&db_path);
    let dir = containing_dir(&db_path);
    let tmp = temp_dir();

    Some(DiskReport {
        db_bytes: file_len(&db_path),
        wal_bytes: file_len(&wal_path),
        db_fs_free: free_space(dir),
        temp_fs_free: free_space(&tmp),
        same_filesystem: same_filesystem(dir, &tmp),
        db_path: db_path.to_string_lossy().into_owned(),
        temp_path: tmp.to_string_lossy().into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn feasible_when_both_filesystems_have_headroom() {
        let v = feasibility(
            8 * GIB,
            Some(20 * GIB),
            Some(20 * GIB),
            "/data",
            "/tmp",
            false,
        );
        assert!(matches!(v, VacuumFeasibility::Ok));
    }

    #[test]
    fn requires_twenty_percent_margin_over_database_size() {
        // 8 GiB db needs 9.6 GiB; 9 GiB is not enough.
        let v = feasibility(
            8 * GIB,
            Some(9 * GIB),
            Some(50 * GIB),
            "/data",
            "/tmp",
            false,
        );
        match v {
            VacuumFeasibility::Insufficient {
                needed,
                available,
                path,
            } => {
                assert_eq!(needed, 8 * GIB + (8 * GIB) / 5);
                assert_eq!(available, 9 * GIB);
                assert_eq!(path, "/data");
            }
            other => panic!("expected Insufficient, got {other:?}"),
        }
    }

    #[test]
    fn reports_the_temp_filesystem_when_it_is_the_binding_constraint() {
        let v = feasibility(8 * GIB, Some(50 * GIB), Some(GIB), "/data", "/tmp", false);
        match v {
            VacuumFeasibility::Insufficient {
                available, path, ..
            } => {
                assert_eq!(available, GIB);
                assert_eq!(path, "/tmp");
            }
            other => panic!("expected Insufficient, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_database_is_always_feasible() {
        assert!(matches!(
            feasibility(0, Some(0), Some(0), "/data", "/tmp", false),
            VacuumFeasibility::Ok
        ));
    }

    #[test]
    fn unknown_when_a_filesystem_cannot_be_measured() {
        let v = feasibility(8 * GIB, None, Some(50 * GIB), "/data", "/tmp", false);
        assert!(matches!(v, VacuumFeasibility::Unknown { ref path } if path == "/data"));

        let v = feasibility(8 * GIB, Some(50 * GIB), None, "/data", "/tmp", false);
        assert!(matches!(v, VacuumFeasibility::Unknown { ref path } if path == "/tmp"));
    }

    #[test]
    fn same_filesystem_requires_the_higher_margin() {
        // 8 GiB db on one volume needs 8 + 8*1.2 = 17.6 GiB.
        let v = feasibility(
            8 * GIB,
            Some(18 * GIB),
            Some(18 * GIB),
            "/data",
            "/data",
            true,
        );
        assert!(matches!(v, VacuumFeasibility::Ok));

        let v = feasibility(
            8 * GIB,
            Some(17 * GIB),
            Some(17 * GIB),
            "/data",
            "/data",
            true,
        );
        assert!(matches!(v, VacuumFeasibility::Insufficient { .. }));
    }

    #[test]
    fn reviewer_scenario_ten_gib_database_fifteen_gib_free_same_volume_is_refused() {
        let v = feasibility(
            10 * GIB,
            Some(15 * GIB),
            Some(15 * GIB),
            "/data",
            "/data",
            true,
        );
        match v {
            VacuumFeasibility::Insufficient {
                needed, available, ..
            } => {
                assert_eq!(needed, 22 * GIB);
                assert_eq!(available, 15 * GIB);
            }
            other => panic!("expected Insufficient, got {other:?}"),
        }
    }

    #[test]
    fn reviewer_scenario_wal_pushes_needed_size_past_available_free_space() {
        // 10 GiB database + 8 GiB WAL on one volume with 24 GiB free. Judged
        // solely on `db_bytes` this needs 10 + 12 = 22 GiB and is wrongly
        // green-lit; the WAL has to be folded into the size the requirement
        // is computed from, making it 18 GiB -> 18 + 21.6 = 39.6 GiB needed,
        // which 24 GiB does not cover.
        let db_bytes = 10 * GIB;
        let wal_bytes = 8 * GIB;
        let v = feasibility(
            db_bytes + wal_bytes,
            Some(24 * GIB),
            Some(24 * GIB),
            "/data",
            "/data",
            true,
        );
        match v {
            VacuumFeasibility::Insufficient {
                needed, available, ..
            } => {
                assert_eq!(needed, 18 * GIB + (18 * GIB * 12) / 10);
                assert_eq!(available, 24 * GIB);
            }
            other => panic!("expected Insufficient, got {other:?}"),
        }
    }

    #[test]
    fn containing_dir_falls_back_to_dot_for_a_bare_filename() {
        assert_eq!(containing_dir(Path::new("happyview.db")), Path::new("."));
        assert_eq!(
            containing_dir(Path::new("/data/happyview.db")),
            Path::new("/data")
        );
    }

    #[test]
    fn wal_path_appends_suffix_without_touching_the_extension() {
        assert_eq!(
            wal_path_for(Path::new("/data/happyview.db")),
            PathBuf::from("/data/happyview.db-wal")
        );
        assert_eq!(
            wal_path_for(Path::new("/data/hv")),
            PathBuf::from("/data/hv-wal")
        );
    }

    #[test]
    fn free_space_returns_none_when_the_path_does_not_exist() {
        assert_eq!(
            free_space(Path::new("/definitely/does/not/exist/at/all")),
            None
        );
    }
}
