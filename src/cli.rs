/// Entry points for `cv branch` and `cv checkout`
pub mod branches;

/// Entry point for `cv init`
pub mod init;

/// Entry point for `cv log`
pub mod log;

/// Entry point for misc low-level commands including `cv cat-file`, `cv object-hash` and `cv ls-tree`
pub mod objects;

/// Entry point for `cv reflog`
pub mod ref_log;

/// Entry point for `cv tag` and `cv show-ref`
pub mod refs;

/// Entry point for `cv remote`
pub mod remotes;

/// Entry point for `cv add`, `cv commit` and `cv check-ignore`
pub mod staging;
