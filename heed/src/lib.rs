#![doc(
    html_favicon_url = "https://raw.githubusercontent.com/meilisearch/heed/main/assets/heed-pigeon.ico?raw=true"
)]
#![doc(
    html_logo_url = "https://raw.githubusercontent.com/meilisearch/heed/main/assets/heed-pigeon-logo.png?raw=true"
)]

//! Crate `heed` is a high-level wrapper of [LMDB], high-level doesn't mean heavy (think about Rust).
//!
//! It provides you a way to store types in LMDB without any limit and with a minimal overhead as possible,
//! relying on the [bytemuck] library to avoid copying bytes when that's unnecessary and the serde library
//! when this is unavoidable.
//!
//! The Lightning Memory-Mapped Database (LMDB) directly maps files parts into main memory, combined
//! with the bytemuck library allows us to safely zero-copy parse and serialize Rust types into LMDB.
//!
//! [LMDB]: https://en.wikipedia.org/wiki/Lightning_Memory-Mapped_Database
//!
//! # Examples
//!
//! Open a database, that will support some typed key/data and ensures, at compile time,
//! that you'll write those types and not others.
//!
//! ```
//! use std::fs;
//! use std::path::Path;
//! use heed::{EnvOpenOptions, Database};
//! use heed::types::*;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let dir = tempfile::tempdir()?;
//! let env = EnvOpenOptions::new().open(dir.path())?;
//!
//! // we will open the default unnamed database
//! let mut wtxn = env.write_txn()?;
//! let db: Database<Str, U32<byteorder::NativeEndian>> = env.create_database(&mut wtxn, None)?;
//!
//! // opening a write transaction
//! db.put(&mut wtxn, "seven", &7)?;
//! db.put(&mut wtxn, "zero", &0)?;
//! db.put(&mut wtxn, "five", &5)?;
//! db.put(&mut wtxn, "three", &3)?;
//! wtxn.commit()?;
//!
//! // opening a read transaction
//! // to check if those values are now available
//! let mut rtxn = env.read_txn()?;
//!
//! let ret = db.get(&rtxn, "zero")?;
//! assert_eq!(ret, Some(0));
//!
//! let ret = db.get(&rtxn, "five")?;
//! assert_eq!(ret, Some(5));
//! # Ok(()) }
//! ```
#![warn(missing_docs)]

mod cursor;
mod database;
mod env;
pub mod iteration_method;
mod iterator;
mod mdb;
mod reserved_space;
mod txn;

use std::ffi::CStr;
use std::{error, fmt, io, mem, result};

use heed_traits as traits;
pub use {bytemuck, byteorder, heed_types as types};

use self::cursor::{RoCursor, RwCursor};
pub use self::database::{Database, DatabaseOpenOptions};
pub use self::env::{
    env_closing_event, CompactionOption, DefaultComparator, Env, EnvClosingEvent, EnvInfo,
    EnvOpenOptions,
};
pub use self::iterator::{
    RoIter, RoPrefix, RoRange, RoRevIter, RoRevPrefix, RoRevRange, RwIter, RwPrefix, RwRange,
    RwRevIter, RwRevPrefix, RwRevRange,
};
pub use self::mdb::error::Error as MdbError;
use self::mdb::ffi::{from_val, into_val};
pub use self::mdb::flags::{DatabaseFlags, EnvFlags, PutFlags};
pub use self::reserved_space::ReservedSpace;
pub use self::traits::{BoxedError, BytesDecode, BytesEncode, Comparator, LexicographicComparator};
pub use self::txn::{RoTxn, RwTxn};

/// The underlying LMDB library version information.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LmdbVersion {
    /// The library version as a string.
    pub string: &'static str,
    /// The library major version number.
    pub major: i32,
    /// The library minor version number.
    pub minor: i32,
    /// The library patch version number.
    pub patch: i32,
}

/// Return the LMDB library version information.
///
/// ```
/// use heed::{lmdb_version, LmdbVersion};
///
/// let expected = LmdbVersion {
///     string: "LMDB 0.9.70: (December 19, 2015)",
///     major: 0,
///     minor: 9,
///     patch: 70,
/// };
/// assert_eq!(lmdb_version(), expected);
/// ```
pub fn lmdb_version() -> LmdbVersion {
    let mut major = mem::MaybeUninit::uninit();
    let mut minor = mem::MaybeUninit::uninit();
    let mut patch = mem::MaybeUninit::uninit();

    unsafe {
        let string_ptr =
            mdb::ffi::mdb_version(major.as_mut_ptr(), minor.as_mut_ptr(), patch.as_mut_ptr());
        LmdbVersion {
            string: CStr::from_ptr(string_ptr).to_str().unwrap(),
            major: major.assume_init(),
            minor: minor.assume_init(),
            patch: patch.assume_init(),
        }
    }
}

/// An error that encapsulates all possible errors in this crate.
#[derive(Debug)]
pub enum Error {
    /// I/O error: can come from the std or be a rewrapped [`MdbError`]
    Io(io::Error),
    /// Lmdb error
    Mdb(MdbError),
    /// Encoding error
    Encoding(BoxedError),
    /// Decoding error
    Decoding(BoxedError),
    /// Incoherent types when opening a database
    InvalidDatabaseTyping,
    /// Database closing in progress
    DatabaseClosing,
    /// Attempt to open Env with different options
    BadOpenOptions {
        /// The options that were used to originally open this env.
        options: EnvOpenOptions,
        /// The env opened with the original options.
        env: Env,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Io(error) => write!(f, "{}", error),
            Error::Mdb(error) => write!(f, "{}", error),
            Error::Encoding(error) => write!(f, "error while encoding: {}", error),
            Error::Decoding(error) => write!(f, "error while decoding: {}", error),
            Error::InvalidDatabaseTyping => {
                f.write_str("database was previously opened with different types")
            }
            Error::DatabaseClosing => {
                f.write_str("database is in a closing phase, you can't open it at the same time")
            }
            Error::BadOpenOptions { .. } => {
                f.write_str("an environment is already opened with different options")
            }
        }
    }
}

impl error::Error for Error {}

impl From<MdbError> for Error {
    fn from(error: MdbError) -> Error {
        match error {
            MdbError::Other(e) => Error::Io(io::Error::from_raw_os_error(e)),
            _ => Error::Mdb(error),
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Error {
        Error::Io(error)
    }
}

/// Either a success or an [`Error`].
pub type Result<T> = result::Result<T, Error>;

/// An unspecified type.
///
/// It is used as placeholders when creating a database.
/// It does not implement the [`BytesEncode`] and [`BytesDecode`] traits
/// and therefore can't be used as codecs. You must use the [`Database::remap_types`]
/// to properly define them.
pub enum Unspecified {}

macro_rules! assert_eq_env_db_txn {
    ($database:ident, $txn:ident) => {
        assert!(
            $database.env_ident == $txn.env_mut_ptr() as usize,
            "The database environment doesn't match the transaction's environment"
        );
    };
}

macro_rules! assert_eq_env_txn {
    ($env:expr, $txn:ident) => {
        assert!(
            $env.env_mut_ptr() == $txn.env_mut_ptr(),
            "The environment doesn't match the transaction's environment"
        );
    };
}

pub(crate) use {assert_eq_env_db_txn, assert_eq_env_txn};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_is_send_sync() {
        fn give_me_send_sync<T: Send + Sync>(_: T) {}

        let error = Error::Encoding(Box::from("There is an issue, you know?"));
        give_me_send_sync(error);
    }
}
