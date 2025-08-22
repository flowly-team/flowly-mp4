pub mod error;
use error::BoxError;
pub use error::Error;

pub type Result<T> = std::result::Result<T, BoxError>;

mod types;
pub use types::*;

mod mp4box;
pub use mp4box::*;

mod file;

mod track;
pub use track::Mp4Track;

pub use file::*;
