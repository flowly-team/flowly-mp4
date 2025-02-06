use bytes::Bytes;
use futures::Future;
use std::collections::{BTreeSet, HashMap};
use std::iter::FromIterator;
use std::ops::Range;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt, SeekFrom};

use crate::error::{BoxError, MemoryStorageError};
use crate::{BlockReader, BoxHeader, BoxType, EmsgBox, FtypBox, MoofBox, MoovBox};
use crate::{Mp4Track, HEADER_SIZE};

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
    type Error = MemoryStorageError;
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
        let buff = self
            .data
            .get(*id)
            .ok_or(MemoryStorageError::DataBufferNotFound(*id))?;

        Ok(buff.slice(range.start as usize..range.end as usize))
    }
}

enum DataBlockBody {
    Memory(Bytes),
    Reader,
}

pub struct DataBlock {
    kind: BoxType,
    offset: u64,
    size: u64,
    buffer: DataBlockBody,
}

pub struct Mp4File<'a, R>
where
    R: AsyncRead + AsyncSeek + Unpin,
{
    pub ftyp: Option<FtypBox>,
    pub emsgs: Vec<EmsgBox>,
    pub tracks: HashMap<u32, Mp4Track>,
    pub reader: &'a mut R,
    pub offsets: BTreeSet<u64>,
    pub data_blocks: Vec<DataBlock>,
}

impl<'a, R> Mp4File<'a, R>
where
    R: AsyncRead + Unpin + AsyncSeek + 'a,
{
    pub fn new(reader: &'a mut R) -> Self {
        Self {
            ftyp: None,
            emsgs: Vec::new(),
            tracks: HashMap::new(),
            reader,
            offsets: BTreeSet::new(),
            data_blocks: Vec::new(),
        }
    }
}

impl<'a, R> Mp4File<'a, R>
where
    R: AsyncRead + Unpin + AsyncSeek + 'a,
{
    pub async fn read_header(&mut self) -> Result<bool, BoxError> {
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
                    println!("ftyp");

                    if buff.len() < s as usize {
                        buff.resize(s as usize, 0);
                    }
                    self.reader.read_exact(&mut buff[0..s as usize]).await?;
                    offset += s;

                    self.ftyp = Some(FtypBox::read_block(&mut &buff[0..s as usize])?);
                }

                BoxType::MoovBox => {
                    println!("moov");

                    if buff.len() < s as usize {
                        buff.resize(s as usize, 0);
                    }

                    self.reader.read_exact(&mut buff[0..s as usize]).await?;
                    offset += s;

                    got_moov = true;
                    self.set_moov(MoovBox::read_block(&mut &buff[0..s as usize])?)?;
                }

                BoxType::MoofBox => {
                    println!("moof");

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
                    println!("emsg");

                    if buff.len() < s as usize {
                        buff.resize(s as usize, 0);
                    }

                    self.reader.read_exact(&mut buff[0..s as usize]).await?;
                    offset += s;

                    self.emsgs
                        .push(EmsgBox::read_block(&mut &buff[0..s as usize])?);
                }

                BoxType::MdatBox => {
                    println!("mdat");
                    self.save_box(BoxType::MdatBox, s, offset).await?;
                    offset += s;
                }

                bt => {
                    println!("{}", bt);

                    self.skip_box(bt, s).await?;
                    offset += s;
                }
            }
            println!("\n");
        }

        Ok(got_moov)
    }

    async fn skip_box(&mut self, bt: BoxType, size: u64) -> Result<(), BoxError> {
        println!("skip {:?}", bt);
        self.reader.seek(SeekFrom::Current(size as _)).await?;
        Ok(())
    }

    async fn save_box(&mut self, kind: BoxType, size: u64, offset: u64) -> Result<(), BoxError> {
        println!("data_block {:?} {} - {}", kind, offset, offset + size);

        if size < 128 * 1024 * 1024 {
            let mut buffer = Vec::new();
            tokio::io::copy(&mut self.reader.take(size), &mut buffer).await?;
            self.data_blocks.push(DataBlock {
                kind,
                offset,
                size,
                buffer: DataBlockBody::Memory(buffer.into()),
            });
        } else {
            self.skip_box(kind, size).await?;
            self.data_blocks.push(DataBlock {
                kind,
                offset,
                size,
                buffer: DataBlockBody::Reader,
            });
        }

        Ok(())
    }

    fn set_moov(&mut self, moov: MoovBox) -> Result<(), BoxError> {
        for trak in moov.traks {
            self.tracks
                .insert(trak.tkhd.track_id, Mp4Track::new(trak, &mut self.offsets)?);
        }

        Ok(())
    }

    fn add_moof(&mut self, offset: u64, moof: MoofBox) -> Result<(), BoxError> {
        for traf in moof.trafs {
            let track_id = traf.tfhd.track_id;

            if let Some(track) = self.tracks.get_mut(&track_id) {
                track.add_traf(offset, moof.mfhd.sequence_number, traf, &mut self.offsets)
            } else {
                return Err(BoxError::TrakNotFound(track_id).into());
            }
        }

        Ok(())
    }

    #[inline]
    pub async fn read_sample_data(
        &mut self,
        track_id: u32,
        sample_idx: usize,
    ) -> Result<Option<Bytes>, BoxError> {
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
                        mem.slice(offset as usize..offset as usize + sample.size as usize)
                    }
                    DataBlockBody::Reader => {
                        let mut buff = vec![0u8; sample.size as _];
                        self.reader.seek(SeekFrom::Start(sample.offset)).await?;
                        self.reader.read_exact(&mut buff).await?;
                        Bytes::from_iter(buff)
                    }
                }));
            }
        }

        Ok(None)
    }
}
