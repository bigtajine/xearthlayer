//! Inode management for the fuse3 filesystem.
//!
//! Re-exports the thread-safe [`InodeManager`] from [`crate::fuse::support`].

pub use crate::fuse::support::inode::InodeManager;
