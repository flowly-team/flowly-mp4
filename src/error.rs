use crate::mp4box::BoxType;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    IoError(#[from] std::io::Error),

    #[error("{0}")]
    InvalidData(&'static str),

    #[error("{0} not found")]
    BoxNotFound(BoxType),

    #[error("{0} and {1} not found")]
    Box2NotFound(BoxType, BoxType),

    #[error("trak[{0}].{1} not found")]
    BoxInTrakNotFound(u32, BoxType),

    #[error("traf[{0}].{1} not found")]
    BoxInTrafNotFound(u32, BoxType),

    #[error("trak[{0}].stbl.{1} not found")]
    BoxInStblNotFound(u32, BoxType),

    #[error("trak[{0}].stbl.{1}.entry[{2}] not found")]
    EntryInStblNotFound(u32, BoxType, u32),

    #[error("traf[{0}].trun.{1}.entry[{2}] not found")]
    EntryInTrunNotFound(u32, BoxType, u32),

    #[error("{0} version {1} is not supported")]
    UnsupportedBoxVersion(BoxType, u8),

    #[error("trak[{0}] not found")]
    TrakNotFound(u32),

    #[error("data buffer with index {0} not found")]
    DataBufferNotFound(usize),

    #[error("failed read length delimeted nalu data")]
    NaluLengthDelimetedRedFail,

    #[error("unsupported media type")]
    UnsupportedMediaType,
}
