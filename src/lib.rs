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
// mod async_reader;
// pub use async_reader::{AsyncMp4Reader, Mp4Header};

// pub async fn read_mp4(f: File) -> Result<Mp4Reader<BufReader<File>>> {
//     let size = f.metadata()?.len();
//     let reader = BufReader::new(f);
//     let mp4 = async_reader::Mp4AsyncReader::read_header(reader, size)?;
//     Ok(mp4)
// }
