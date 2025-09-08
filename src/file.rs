use bytes::Bytes;
use futures::Future;
use std::collections::{BTreeSet, HashMap};
use std::convert::TryInto;
use std::iter::FromIterator;
use std::ops::Range;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt, SeekFrom};

use crate::{BlockReader, BoxHeader, BoxType, EmsgBox, Error, FtypBox, MoofBox, MoovBox};
use crate::{Mp4Track, HEADER_SIZE};

const MAX_MEM_MDAT_SIZE: u64 = 128 * 1024 * 1024; // 128mb

pub trait DataStorage {
    type Error;
    type Id;

    fn save_data(
        &mut self,
        reader: &mut (impl AsyncRead + Unpin),
    ) -> impl Future<Output = Result<Self::Id, Self::Error>>;

    fn read_data(
        &self,
        id: &Self::Id,
        range: Range<u64>,
    ) -> impl Future<Output = Result<Bytes, Self::Error>>;
}

#[derive(Default)]
pub struct MemoryStorage {
    pub data: Vec<Bytes>,
}

impl DataStorage for MemoryStorage {
    type Error = Error;
    type Id = usize;

    #[inline]
    async fn save_data(
        &mut self,
        reader: &mut (impl AsyncRead + Unpin),
    ) -> Result<Self::Id, Self::Error> {
        let mut buffer = Vec::new();
        let index = self.data.len();
        tokio::io::copy(reader, &mut buffer).await?;
        self.data.push(buffer.into());

        Ok(index)
    }

    #[inline]
    async fn read_data(&self, id: &Self::Id, range: Range<u64>) -> Result<Bytes, Self::Error> {
        let buff = self.data.get(*id).ok_or(Error::DataBufferNotFound(*id))?;

        Ok(buff.slice(range.start as usize..range.end as usize))
    }
}

enum DataBlockBody {
    Memory(Bytes),
    Reader,
}

pub struct DataBlock {
    _kind: BoxType,
    offset: u64,
    size: u64,
    buffer: DataBlockBody,
}

pub trait ReadSampleFormat: Default {
    fn format(&self, data: &mut [u8]) -> Result<(), Error>;
}

#[derive(Default)]
pub struct Annexb {}

impl ReadSampleFormat for Annexb {
    fn format(&self, data: &mut [u8]) -> Result<(), Error> {
        // TODO:
        // * For each IDR frame, copy the SPS and PPS from the stream's
        //   parameters, rather than depend on it being present in the frame
        //   already. In-band parameters aren't guaranteed. This is awkward
        //   with h264_reader v0.5's h264_reader::avcc::AvcDecoderRecord because it
        //   strips off the NAL header byte from each parameter. The next major
        //   version shouldn't do this.
        // * Copy only the slice data. In particular, don't copy SEI, which confuses
        //   Safari: <https://github.com/scottlamb/retina/issues/60#issuecomment-1178369955>

        let mut i = 0;
        while i < data.len() - 3 {
            // Replace each NAL's length with the Annex B start code b"\x00\x00\x00\x01".
            let bytes = &mut data[i..i + 4];
            let nalu_length = u32::from_be_bytes(bytes.try_into().unwrap()) as usize;
            bytes.copy_from_slice(&[0, 0, 0, 1]);

            i += 4 + nalu_length;

            if i > data.len() {
                return Err(Error::NaluLengthDelimetedRedFail);
            }
        }

        if i < data.len() {
            return Err(Error::NaluLengthDelimetedRedFail);
        }

        Ok(())
    }
}

#[derive(Default)]
pub struct LengthDelimited {}

impl ReadSampleFormat for LengthDelimited {
    fn format(&self, _data: &mut [u8]) -> Result<(), Error> {
        Ok(())
    }
}

pub struct Mp4File<R, F = Annexb>
where
    R: AsyncRead + AsyncSeek + Unpin,
    F: ReadSampleFormat,
{
    pub ftyp: Option<FtypBox>,
    pub emsgs: Vec<EmsgBox>,
    pub tracks: HashMap<u32, Mp4Track>,
    pub reader: R,
    pub offsets: BTreeSet<u64>,
    pub data_blocks: Vec<DataBlock>,
    format_conv: F,
}

impl<R> Mp4File<R>
where
    R: AsyncRead + Unpin + AsyncSeek,
{
    pub fn new_annexb(reader: R) -> Self {
        Self {
            ftyp: None,
            emsgs: Vec::new(),
            tracks: HashMap::new(),
            reader,
            offsets: BTreeSet::new(),
            data_blocks: Vec::new(),
            format_conv: Default::default(),
        }
    }
}

impl<R> Mp4File<R, LengthDelimited>
where
    R: AsyncRead + Unpin + AsyncSeek,
{
    pub fn new(reader: R) -> Self {
        Self {
            ftyp: None,
            emsgs: Vec::new(),
            tracks: HashMap::new(),
            reader,
            offsets: BTreeSet::new(),
            data_blocks: Vec::new(),
            format_conv: Default::default(),
        }
    }
}

impl<R, F> Mp4File<R, F>
where
    R: AsyncRead + Unpin + AsyncSeek,
    F: ReadSampleFormat,
{
    pub async fn read_header(&mut self) -> Result<bool, Error> {
        let mut buff = Vec::with_capacity(8192);
        let mut got_moov = false;
        let mut offset = 0u64;

        while let Some(BoxHeader { kind, size: mut s }) =
            BoxHeader::read(&mut self.reader, &mut offset).await?
        {
            if s >= HEADER_SIZE {
                s -= HEADER_SIZE; // size without header
            }
            match kind {
                BoxType::FtypBox => {
                    log::debug!("ftyp");

                    if buff.len() < s as usize {
                        buff.resize(s as usize, 0);
                    }
                    self.reader.read_exact(&mut buff[0..s as usize]).await?;
                    offset += s;

                    self.ftyp = Some(FtypBox::read_block(&mut &buff[0..s as usize])?);
                }

                BoxType::MoovBox => {
                    log::debug!("moov");

                    if buff.len() < s as usize {
                        buff.resize(s as usize, 0);
                    }

                    self.reader.read_exact(&mut buff[0..s as usize]).await?;
                    offset += s;

                    got_moov = true;
                    self.set_moov(MoovBox::read_block(&mut &buff[0..s as usize])?)?;
                }

                BoxType::MoofBox => {
                    log::debug!("moof");

                    if buff.len() < s as usize {
                        buff.resize(s as usize, 0);
                    }

                    let begin_offset = offset;
                    self.reader.read_exact(&mut buff[0..s as usize]).await?;
                    offset += s;

                    self.add_moof(
                        begin_offset,
                        MoofBox::read_block(&mut &buff[0..s as usize])?,
                    )?;
                }

                BoxType::EmsgBox => {
                    log::debug!("emsg");

                    if buff.len() < s as usize {
                        buff.resize(s as usize, 0);
                    }

                    self.reader.read_exact(&mut buff[0..s as usize]).await?;
                    offset += s;

                    self.emsgs
                        .push(EmsgBox::read_block(&mut &buff[0..s as usize])?);
                }

                BoxType::MdatBox => {
                    log::debug!("mdat");
                    self.save_box(BoxType::MdatBox, s, offset).await?;
                    offset += s;
                }

                bt => {
                    log::debug!("{}", bt);

                    self.skip_box(bt, s).await?;
                    offset += s;
                }
            }
        }

        Ok(got_moov)
    }

    async fn skip_box(&mut self, bt: BoxType, size: u64) -> Result<(), Error> {
        log::debug!("skip {:?}", bt);
        self.reader.seek(SeekFrom::Current(size as _)).await?;
        Ok(())
    }

    async fn save_box(&mut self, kind: BoxType, size: u64, offset: u64) -> Result<(), Error> {
        log::debug!("data_block {:?} {} - {}", kind, offset, offset + size);
        let reader = &mut self.reader;

        if size < MAX_MEM_MDAT_SIZE {
            let mut buffer = Vec::new();
            tokio::io::copy(&mut reader.take(size), &mut buffer).await?;
            self.data_blocks.push(DataBlock {
                _kind: kind,
                offset,
                size,
                buffer: DataBlockBody::Memory(buffer.into()),
            });
        } else {
            self.skip_box(kind, size).await?;

            self.data_blocks.push(DataBlock {
                _kind: kind,
                offset,
                size,
                buffer: DataBlockBody::Reader,
            });
        }

        Ok(())
    }

    fn set_moov(&mut self, moov: MoovBox) -> Result<(), Error> {
        for trak in moov.traks {
            self.tracks
                .insert(trak.tkhd.track_id, Mp4Track::new(trak, &mut self.offsets)?);
        }

        Ok(())
    }

    fn add_moof(&mut self, offset: u64, moof: MoofBox) -> Result<(), Error> {
        for traf in moof.trafs {
            let track_id = traf.tfhd.track_id;

            if let Some(track) = self.tracks.get_mut(&track_id) {
                track.add_traf(offset, moof.mfhd.sequence_number, traf, &mut self.offsets)
            } else {
                return Err(Error::TrakNotFound(track_id));
            }
        }

        Ok(())
    }

    #[inline]
    pub async fn read_sample_data(
        &mut self,
        track_id: u32,
        sample_idx: usize,
    ) -> Result<Option<Bytes>, Error> {
        let Some(track) = self.tracks.get(&track_id) else {
            return Ok(None);
        };

        let Some(sample) = track.samples.get(sample_idx) else {
            return Ok(None);
        };

        for block in &self.data_blocks {
            let range = block.offset..block.offset + block.size;

            if range.contains(&sample.offset) {
                return Ok(Some(match &block.buffer {
                    DataBlockBody::Memory(mem) => {
                        let offset = sample.offset - block.offset;
                        let mut slice = mem
                            .slice(offset as usize..offset as usize + sample.size as usize)
                            .to_vec();

                        self.format_conv.format(&mut slice).unwrap();
                        Bytes::from(slice)
                    }

                    DataBlockBody::Reader => {
                        let mut buff = vec![0u8; sample.size as _];
                        self.reader.seek(SeekFrom::Start(sample.offset)).await?;
                        self.reader.read_exact(&mut buff).await?;
                        self.format_conv.format(&mut buff).unwrap();
                        Bytes::from_iter(buff)
                    }
                }));
            }
        }

        Ok(None)
    }
}

// #[derive(Debug, Clone)]
// pub struct Mp4Demuxer {
//     annexb: bool,
// }

// impl Mp4Demuxer {
//     pub fn new(annexb: bool) -> Self {
//         Self { annexb }
//     }
// }

// impl<F: DataFrame> Service<F> for Mp4Demuxer {
//     type Out = Result<Mp4Frame<F::Source>, Error>;

//     fn handle(
//         &mut self,
//         input: F,
//         cx: &flowly::Context,
//     ) -> impl futures::Stream<Item = Self::Out> + Send {
//         async_stream::stream! {}
//     }
// }
