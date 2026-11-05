//! The ownership-and-mode rule for files and directories that hold secrets
//! or feed code to the process: this user owns it and nobody else holds a
//! permission bit on it.

use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NotPrivate {
    OtherOwner { uid: u32, euid: u32 },
    OpenMode { mode: u32 },
}

/// The rule against an explicit uid, so a test can stand in for another user.
pub(crate) fn private_to(metadata: &Metadata, euid: u32) -> Result<(), NotPrivate> {
    if metadata.uid() != euid {
        return Err(NotPrivate::OtherOwner {
            uid: metadata.uid(),
            euid,
        });
    }
    let mode = metadata.mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(NotPrivate::OpenMode { mode });
    }
    Ok(())
}

pub(crate) fn private_to_this_user(metadata: &Metadata) -> Result<(), NotPrivate> {
    // SAFETY: `geteuid` reads a process attribute and cannot fail.
    private_to(metadata, unsafe { libc::geteuid() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn names_the_owner_and_the_mode() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let metadata = std::fs::metadata(dir.path()).unwrap();
        assert_eq!(private_to_this_user(&metadata), Ok(()));
        assert!(matches!(
            private_to(&metadata, metadata.uid().wrapping_add(1)),
            Err(NotPrivate::OtherOwner { .. })
        ));
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o750)).unwrap();
        let metadata = std::fs::metadata(dir.path()).unwrap();
        assert_eq!(
            private_to_this_user(&metadata),
            Err(NotPrivate::OpenMode { mode: 0o750 })
        );
    }
}
