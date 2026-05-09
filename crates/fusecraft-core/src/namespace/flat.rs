//! Flat namespace layout: all files in a single directory with zero-padded decimal names.

use std::ffi::OsStr;
use std::time::SystemTime;

use crate::config::FilesConfig;
use crate::error::FsError;

use super::{DirSink, FUSE_ROOT_INO, FileAttrSpec, FileKind, NamespaceModel};

/// Maximum files supported by the 6-digit zero-padded name scheme.
const MAX_FILES: u64 = 1_000_000;

/// A flat namespace with `inode_count` files in a single `objects/` directory.
///
/// Layout:
/// - `/` (ino 1): contains only `objects/`.
/// - `/objects` (ino 2): contains `000000`..`(inode_count-1)`.
/// - `/objects/<NNNNNN>` (ino 100 + n): synthetic files of `file_size_bytes`.
///
/// If `inode_count` exceeds 999_999 it is silently clamped to 999_999 because
/// the 6-digit naming scheme cannot represent larger indices.
#[derive(Debug, Clone)]
pub struct FlatObjectNamespace {
    inode_count: u64,
    file_size_bytes: u64,
    uid: u32,
    gid: u32,
    ctime: SystemTime,
}

impl FlatObjectNamespace {
    /// Inode of the top-level `objects/` directory.
    pub const OBJECTS_INO: u64 = 2;
    /// Base inode for object files. File index `n` maps to `FIRST_FILE_INO + n`.
    pub const FIRST_FILE_INO: u64 = 100;

    /// Create a new flat namespace from configuration.
    pub fn new(config: &FilesConfig) -> Self {
        let inode_count = config.inode_count.min(MAX_FILES - 1);
        if config.inode_count >= MAX_FILES {
            eprintln!(
                "fusecraft: inode_count ({}) exceeds maximum {}; clamping to {}",
                config.inode_count,
                MAX_FILES - 1,
                inode_count
            );
        }
        // SAFETY: getuid/getgid are always-safe POSIX calls that read process credentials.
        let (uid, gid) = unsafe { (libc::getuid() as u32, libc::getgid() as u32) };
        Self {
            inode_count,
            file_size_bytes: config.file_size_bytes,
            uid,
            gid,
            ctime: SystemTime::now(),
        }
    }

    /// Total number of object files in the namespace.
    pub fn inode_count(&self) -> u64 {
        self.inode_count
    }

    /// File size configured for all synthetic files.
    pub fn file_size_bytes(&self) -> u64 {
        self.file_size_bytes
    }

    /// Convert a file index `n` to its 6-digit zero-padded name.
    pub fn index_to_name(n: u64) -> String {
        format!("{n:06}")
    }

    /// Convert an inode to a file index, if it's a valid file inode.
    pub fn ino_to_index(&self, ino: u64) -> Option<u64> {
        let n = ino.checked_sub(Self::FIRST_FILE_INO)?;
        (n < self.inode_count).then_some(n)
    }

    /// Parse a 6-digit zero-padded decimal name to its index.
    ///
    /// Returns `None` if the name is not exactly 6 ASCII digits or the index
    /// is out of range.
    fn parse_name(&self, name: &OsStr) -> Option<u64> {
        let s = name.to_str()?;
        if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let n: u64 = s.parse().ok()?;
        (n < self.inode_count).then_some(n)
    }

    fn dir_attr(&self, ino: u64) -> FileAttrSpec {
        FileAttrSpec {
            ino,
            size: 0,
            kind: FileKind::Dir,
            mode: 0o755,
            nlink: 2,
            uid: self.uid,
            gid: self.gid,
            mtime: self.ctime,
        }
    }

    fn file_attr(&self, ino: u64) -> FileAttrSpec {
        FileAttrSpec {
            ino,
            size: self.file_size_bytes,
            kind: FileKind::File,
            mode: 0o644,
            nlink: 1,
            uid: self.uid,
            gid: self.gid,
            mtime: self.ctime,
        }
    }
}

impl NamespaceModel for FlatObjectNamespace {
    fn lookup(&self, parent: u64, name: &OsStr) -> Option<u64> {
        match parent {
            FUSE_ROOT_INO => (name == OsStr::new("objects")).then_some(Self::OBJECTS_INO),
            Self::OBJECTS_INO => self.parse_name(name).map(|n| Self::FIRST_FILE_INO + n),
            _ => None,
        }
    }

    fn attr(&self, ino: u64) -> Option<FileAttrSpec> {
        match ino {
            FUSE_ROOT_INO => Some(self.dir_attr(FUSE_ROOT_INO)),
            Self::OBJECTS_INO => Some(self.dir_attr(Self::OBJECTS_INO)),
            ino if self.ino_to_index(ino).is_some() => Some(self.file_attr(ino)),
            _ => None,
        }
    }

    fn readdir(&self, ino: u64, offset: u64, sink: &mut dyn DirSink) -> Result<(), FsError> {
        match ino {
            FUSE_ROOT_INO => {
                // Entries: "." @1, ".." @2, "objects" @3.
                let entries: [(u64, u64, FileKind, &OsStr); 3] = [
                    (1, FUSE_ROOT_INO, FileKind::Dir, OsStr::new(".")),
                    (2, FUSE_ROOT_INO, FileKind::Dir, OsStr::new("..")),
                    (3, Self::OBJECTS_INO, FileKind::Dir, OsStr::new("objects")),
                ];
                for (off, child_ino, kind, name) in entries {
                    if off <= offset {
                        continue;
                    }
                    if !sink.push(child_ino, off, kind, name) {
                        return Ok(());
                    }
                }
                Ok(())
            }
            Self::OBJECTS_INO => {
                if 1 > offset && !sink.push(Self::OBJECTS_INO, 1, FileKind::Dir, OsStr::new(".")) {
                    return Ok(());
                }
                if 2 > offset && !sink.push(FUSE_ROOT_INO, 2, FileKind::Dir, OsStr::new("..")) {
                    return Ok(());
                }
                // Files start at offset 3.
                let start = offset.saturating_sub(2);
                for n in start..self.inode_count {
                    let off = n + 3;
                    let name = Self::index_to_name(n);
                    if !sink.push(
                        Self::FIRST_FILE_INO + n,
                        off,
                        FileKind::File,
                        OsStr::new(&name),
                    ) {
                        return Ok(());
                    }
                }
                Ok(())
            }
            _ => Err(FsError::Errno(libc::ENOTDIR)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(inode_count: u64) -> FilesConfig {
        FilesConfig {
            inode_count,
            file_size_bytes: 4096,
            ..FilesConfig::default()
        }
    }

    struct CollectingSink {
        entries: Vec<(u64, u64, FileKind, String)>,
        stop_after: Option<usize>,
    }

    impl CollectingSink {
        fn unbounded() -> Self {
            Self {
                entries: Vec::new(),
                stop_after: None,
            }
        }
        fn stop_after(n: usize) -> Self {
            Self {
                entries: Vec::new(),
                stop_after: Some(n),
            }
        }
    }

    impl DirSink for CollectingSink {
        fn push(&mut self, ino: u64, offset: u64, kind: FileKind, name: &OsStr) -> bool {
            self.entries
                .push((ino, offset, kind, name.to_str().unwrap().to_string()));
            match self.stop_after {
                Some(n) => self.entries.len() < n,
                None => true,
            }
        }
    }

    #[test]
    fn lookup_objects_under_root() {
        let ns = FlatObjectNamespace::new(&test_config(10));
        let ino = ns
            .lookup(FUSE_ROOT_INO, OsStr::new("objects"))
            .expect("objects found");
        assert_eq!(ino, FlatObjectNamespace::OBJECTS_INO);
    }

    #[test]
    fn lookup_rejects_wrong_name_under_root() {
        let ns = FlatObjectNamespace::new(&test_config(10));
        assert!(ns.lookup(FUSE_ROOT_INO, OsStr::new("other")).is_none());
    }

    #[test]
    fn lookup_file_under_objects() {
        let ns = FlatObjectNamespace::new(&test_config(100));
        let ino = ns
            .lookup(FlatObjectNamespace::OBJECTS_INO, OsStr::new("000042"))
            .expect("file found");
        assert_eq!(ino, FlatObjectNamespace::FIRST_FILE_INO + 42);
    }

    #[test]
    fn lookup_rejects_invalid_names() {
        let ns = FlatObjectNamespace::new(&test_config(100));
        let objects = FlatObjectNamespace::OBJECTS_INO;
        assert!(ns.lookup(objects, OsStr::new("00042")).is_none()); // 5 digits
        assert!(ns.lookup(objects, OsStr::new("0000042")).is_none()); // 7 digits
        assert!(ns.lookup(objects, OsStr::new("abcdef")).is_none());
        assert!(ns.lookup(objects, OsStr::new("00004a")).is_none());
    }

    #[test]
    fn lookup_rejects_out_of_range() {
        let ns = FlatObjectNamespace::new(&test_config(10));
        assert!(
            ns.lookup(FlatObjectNamespace::OBJECTS_INO, OsStr::new("000010"))
                .is_none()
        );
        assert!(
            ns.lookup(FlatObjectNamespace::OBJECTS_INO, OsStr::new("000099"))
                .is_none()
        );
    }

    #[test]
    fn lookup_under_unknown_parent() {
        let ns = FlatObjectNamespace::new(&test_config(10));
        assert!(ns.lookup(42, OsStr::new("anything")).is_none());
    }

    #[test]
    fn attr_root_is_dir() {
        let ns = FlatObjectNamespace::new(&test_config(10));
        let a = ns.attr(FUSE_ROOT_INO).expect("root attr");
        assert_eq!(a.ino, FUSE_ROOT_INO);
        assert!(matches!(a.kind, FileKind::Dir));
        assert_eq!(a.mode, 0o755);
        assert_eq!(a.nlink, 2);
    }

    #[test]
    fn attr_objects_is_dir() {
        let ns = FlatObjectNamespace::new(&test_config(10));
        let a = ns.attr(FlatObjectNamespace::OBJECTS_INO).unwrap();
        assert!(matches!(a.kind, FileKind::Dir));
    }

    #[test]
    fn attr_file_has_configured_size() {
        let ns = FlatObjectNamespace::new(&test_config(10));
        let a = ns.attr(FlatObjectNamespace::FIRST_FILE_INO + 5).unwrap();
        assert!(matches!(a.kind, FileKind::File));
        assert_eq!(a.size, 4096);
        assert_eq!(a.mode, 0o644);
        assert_eq!(a.nlink, 1);
    }

    #[test]
    fn attr_unknown_inode() {
        let ns = FlatObjectNamespace::new(&test_config(10));
        assert!(ns.attr(FlatObjectNamespace::FIRST_FILE_INO + 10).is_none());
        assert!(ns.attr(50).is_none());
    }

    #[test]
    fn readdir_root_yields_dot_dotdot_objects() {
        let ns = FlatObjectNamespace::new(&test_config(10));
        let mut sink = CollectingSink::unbounded();
        ns.readdir(FUSE_ROOT_INO, 0, &mut sink).unwrap();
        assert_eq!(sink.entries.len(), 3);
        assert_eq!(sink.entries[0].3, ".");
        assert_eq!(sink.entries[1].3, "..");
        assert_eq!(sink.entries[2].3, "objects");
        assert_eq!(sink.entries[2].1, 3);
    }

    #[test]
    fn readdir_root_respects_offset() {
        let ns = FlatObjectNamespace::new(&test_config(10));
        let mut sink = CollectingSink::unbounded();
        ns.readdir(FUSE_ROOT_INO, 2, &mut sink).unwrap();
        assert_eq!(sink.entries.len(), 1);
        assert_eq!(sink.entries[0].3, "objects");
    }

    #[test]
    fn readdir_objects_yields_all_entries() {
        let ns = FlatObjectNamespace::new(&test_config(5));
        let mut sink = CollectingSink::unbounded();
        ns.readdir(FlatObjectNamespace::OBJECTS_INO, 0, &mut sink)
            .unwrap();
        // ".", "..", and 5 files.
        assert_eq!(sink.entries.len(), 7);
        assert_eq!(sink.entries[0].3, ".");
        assert_eq!(sink.entries[1].3, "..");
        for (idx, (ino, off, kind, name)) in sink.entries[2..].iter().enumerate() {
            assert_eq!(*name, format!("{:06}", idx));
            assert_eq!(*ino, FlatObjectNamespace::FIRST_FILE_INO + idx as u64);
            assert_eq!(*off, 3 + idx as u64);
            assert!(matches!(kind, FileKind::File));
        }
    }

    #[test]
    fn readdir_objects_respects_offset() {
        let ns = FlatObjectNamespace::new(&test_config(10));
        let mut sink = CollectingSink::unbounded();
        // FUSE cookie semantics: offset=4 means resume AFTER the entry at offset 4.
        // Entries: "." @1, ".." @2, "000000" @3, "000001" @4, "000002" @5, ...
        // So the first emitted entry is "000002" at offset 5.
        ns.readdir(FlatObjectNamespace::OBJECTS_INO, 4, &mut sink)
            .unwrap();
        assert_eq!(sink.entries[0].3, "000002");
        assert_eq!(sink.entries[0].1, 5);
    }

    #[test]
    fn readdir_stops_when_sink_returns_false() {
        let ns = FlatObjectNamespace::new(&test_config(100));
        let mut sink = CollectingSink::stop_after(5);
        ns.readdir(FlatObjectNamespace::OBJECTS_INO, 0, &mut sink)
            .unwrap();
        assert_eq!(sink.entries.len(), 5);
    }

    #[test]
    fn readdir_on_file_returns_enotdir() {
        let ns = FlatObjectNamespace::new(&test_config(10));
        let mut sink = CollectingSink::unbounded();
        let err = ns
            .readdir(FlatObjectNamespace::FIRST_FILE_INO, 0, &mut sink)
            .unwrap_err();
        match err {
            FsError::Errno(e) => assert_eq!(e, libc::ENOTDIR),
            other => panic!("expected ENOTDIR, got {other:?}"),
        }
    }

    #[test]
    fn usable_via_trait_object() {
        let ns: Box<dyn NamespaceModel> = Box::new(FlatObjectNamespace::new(&test_config(3)));
        let ino = ns
            .lookup(FUSE_ROOT_INO, OsStr::new("objects"))
            .expect("lookup through trait");
        assert_eq!(ino, FlatObjectNamespace::OBJECTS_INO);
        let attr = ns.attr(FUSE_ROOT_INO).unwrap();
        assert!(matches!(attr.kind, FileKind::Dir));
    }

    #[test]
    fn clamps_inode_count_at_limit() {
        let mut config = test_config(0);
        config.inode_count = 2_000_000;
        let ns = FlatObjectNamespace::new(&config);
        assert_eq!(ns.inode_count(), MAX_FILES - 1);
    }
}
