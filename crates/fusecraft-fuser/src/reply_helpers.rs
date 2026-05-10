//! Helper functions for converting between fusecraft-core types and fuser types.

use std::time::SystemTime;

use fuser::{FileAttr, FileType, INodeNo};

use fusecraft_core::namespace::{FileAttrSpec, FileKind};

/// Convert a `FileAttrSpec` from the namespace model into a fuser `FileAttr`.
pub(crate) fn attr_from_spec(spec: &FileAttrSpec) -> FileAttr {
    let kind = match spec.kind {
        FileKind::Dir => FileType::Directory,
        FileKind::File => FileType::RegularFile,
    };
    let blocks = spec.size.div_ceil(512);
    FileAttr {
        ino: INodeNo(spec.ino),
        size: spec.size,
        blocks,
        atime: spec.mtime,
        mtime: spec.mtime,
        ctime: spec.mtime,
        crtime: SystemTime::UNIX_EPOCH,
        kind,
        perm: spec.mode as u16,
        nlink: spec.nlink,
        uid: spec.uid,
        gid: spec.gid,
        rdev: 0,
        blksize: 4096,
        flags: 0,
    }
}

/// Convert a raw i32 errno into a fuser `Errno`.
pub(crate) fn errno_to_fuser(errno: i32) -> fuser::Errno {
    fuser::Errno::from_i32(errno)
}
