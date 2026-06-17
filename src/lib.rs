#![warn(missing_docs)]
//! Cait's Version of Version Control.  Git-compatible version control.
//!
//! CVVC is a library crate which can be used to manipulate Git on-disk structures; the package also contains
//! a binary crate which builds a "cv" command-line application which mimics some of the functionality of git(1).
//!
//! # Features
//!
//! At present, CVVC can read and write loose objects, create tags, commits and branches, switch branches, and
//! read from packfiles.
//!
//! # Limitations
//!
//! As CVVC is written in Rust, CVVC is UTF-8-oriented.  Because of this, repositories which contain (for example)
//! commit messages in other text encodings may not interoperate properly with CVVC, if the non-UTF-8 text cannot
//! be cleanly converted.  If your worktree is on a filesystem with non-Unicode filename encodings, you may face
//! issues adding files to the repository if their filenames cannot be cleanly converted to UTF-8.
//!
//! CVVC does not support any network protocols, so can only be used for local repositories.
//!
//! CVVC only supports SHA-1-based repositories.
//!
//! CVVC only supports worktree index version 2
//!
//! CVVC only supports packfile index version 2.  Unindexed packfiles are quietly re-indexed on startup.
//!
//! CVVC does not do any line-ending conversion.
//!
//! Various other Git features are not currently supported; some likely never will be.

/// Entry points for CLI commands, and display logic for CLI output.
pub mod cli;

/// Config file handling
pub mod config;

/// General helper functions
pub mod helpers;

/// Git ignore file parsing
pub mod ignore;

/// Git index file parsing
pub mod index;

/// Server communications
pub mod net;

/// Git object parsing
pub mod objects;

/// Git ref log parsing
pub mod ref_log;

/// Repository management routines
pub mod repo;

/// Storage backends for objects and branches.
pub mod stores;
