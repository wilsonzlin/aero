pub mod io;
pub mod platform;

mod error;
pub use error::{DiskError, DiskResult};

pub use crate::io::snapshot_file::OpfsSyncFile;
pub use crate::io::storage::backends::opfs::{
    OpfsBackend, OpfsBackendMode, OpfsByteStorage, OpfsStorage,
};
