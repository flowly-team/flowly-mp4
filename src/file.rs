use bytes::Bytes;
use futures::Future;
use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::ctts::CttsEntry;
use crate::error::BoxError;
use crate::stsc::StscEntry;
use crate::stts::SttsEntry;
use crate::{
    BlockReader, BoxHeader, BoxType, EmsgBox, Error, FtypBox, MoofBox, MoovBox, Mp4Sample,
    TrackType,
};

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

#[derive(thiserror::Error, Debug)]
pub enum MemoryStorageError {
    #[error("IoError: {0}")]
    IoError(#[from] std::io::Error),

    #[error("data buffer with index {0} not found")]
    DataBufferNotFound(usize),
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

enum DataBlockBody<I> {
    Memory(Bytes),
    Storage(I),
    Reader,
}

pub struct DataBlock<I> {
    kind: BoxType,
    offset: u64,
    size: u64,
    buffer: DataBlockBody<I>,
}

pub struct Mp4File<'a, R, S = MemoryStorage>
where
    R: AsyncRead + Unpin,
    S: DataStorage,
{
    pub ftyp: Option<FtypBox>,
    pub emsgs: Vec<EmsgBox>,
    pub tracks: HashMap<u32, Mp4Track>,
    pub reader: &'a mut R,
    pub offsets: BTreeSet<u64>,
    pub data_blocks: Vec<DataBlock<S::Id>>,
    pub data_storage: S,
}

impl<'a, R> Mp4File<'a, R, MemoryStorage>
where
    R: AsyncRead + Unpin + 'a,
{
    pub fn new(reader: &'a mut R) -> Self {
        Self {
            ftyp: None,
            emsgs: Vec::new(),
            tracks: HashMap::new(),
            reader,
            offsets: BTreeSet::new(),
            data_blocks: Vec::new(),
            data_storage: MemoryStorage::default(),
        }
    }
}

impl<'a, R, S> Mp4File<'a, R, S>
where
    R: AsyncRead + Unpin + 'a,
    S: DataStorage,
{
    pub fn with_storage(reader: &'a mut R, data_storage: S) -> Self {
        Self {
            ftyp: None,
            emsgs: Vec::new(),
            tracks: HashMap::new(),
            reader,
            offsets: BTreeSet::new(),
            data_blocks: Vec::new(),
            data_storage,
        }
    }

    pub async fn read_header(&mut self) -> Result<bool, Error<S::Error>> {
        let mut buff = Vec::with_capacity(8192);
        let mut got_moov = false;
        let mut offset = 0u64;

        while let Some(BoxHeader { kind, size: s }) =
            BoxHeader::read(&mut self.reader, &mut offset).await?
        {
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
        }

        Ok(got_moov)
    }

    async fn skip_box(&mut self, bt: BoxType, size: u64) -> Result<(), Error<S::Error>> {
        println!("skip {:?}", bt);
        tokio::io::copy(&mut (&mut self.reader).take(size), &mut tokio::io::empty()).await?;
        Ok(())
    }

    async fn save_box(
        &mut self,
        kind: BoxType,
        size: u64,
        offset: u64,
    ) -> Result<(), Error<S::Error>> {
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
            let id = self
                .data_storage
                .save_data(&mut self.reader.take(size))
                .await
                .map_err(Error::DataStorageError)?;

            self.data_blocks.push(DataBlock {
                kind,
                offset,
                size,
                buffer: DataBlockBody::Storage(id),
            });
        }

        Ok(())
    }

    fn set_moov(&mut self, moov: MoovBox) -> Result<(), Error<S::Error>> {
        for trak in moov.traks {
            self.tracks
                .insert(trak.tkhd.track_id, Mp4Track::new(trak, &mut self.offsets)?);
        }

        Ok(())
    }

    fn add_moof(&mut self, offset: u64, moof: MoofBox) -> Result<(), Error<S::Error>> {
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
        &self,
        track_id: u32,
        sample_idx: usize,
    ) -> Result<Option<Bytes>, Error<S::Error>> {
        let Some(track) = self.tracks.get(&track_id) else {
            return Ok(None);
        };

        let Some(sample) = track.samples.get(sample_idx) else {
            return Ok(None);
        };

        for block in &self.data_blocks {
            let range = block.offset..block.offset + block.size;

            if range.contains(&sample.offset) {
                let offset = sample.offset - block.offset;

                return Ok(Some(match &block.buffer {
                    DataBlockBody::Storage(id) => self
                        .data_storage
                        .read_data(id, offset..offset + sample.size as u64)
                        .await
                        .map_err(Error::DataStorageError)?,

                    DataBlockBody::Memory(mem) => {
                        mem.slice(offset as usize..offset as usize + sample.size as usize)
                    }
                    DataBlockBody::Reader => todo!(),
                }));
            }
        }

        Ok(None)
    }

    pub fn into_streams<T: AsRef<[u32]>>(
        self,
        tracks: T,
    ) -> impl Iterator<
        Item = (
            u32,
            impl futures::Stream<Item = Result<Mp4Sample, Error<S::Error>>> + 'a,
        ),
    >
    where
        S::Error: 'a,
    {
        let storage = Arc::new(self.data_storage);
        let data_blocks = Arc::new(self.data_blocks);

        self.tracks
            .into_iter()
            .filter_map(move |(track_id, track)| {
                if !tracks.as_ref().contains(&track_id) {
                    return None;
                }

                let storage = storage.clone();
                let data_blocks = data_blocks.clone();

                Some((
                    track_id,
                    async_stream::stream! {
                        for samp_offset in track.samples {
                            yield Ok(Mp4Sample {
                                start_time: samp_offset.start_time,
                                duration: samp_offset.duration,
                                rendering_offset: samp_offset.rendering_offset,
                                is_sync: samp_offset.is_sync,
                                bytes: Bytes::new(),
                            })
                        }
                    },
                ))
            })
    }
}

pub struct Mp4SampleOffset {
    pub offset: u64,
    pub size: u32,
    pub duration: u32,
    pub start_time: u64,
    pub rendering_offset: i32,
    pub is_sync: bool,
    pub chunk_id: u32,
}

pub struct Mp4Track {
    pub track_id: u32,
    pub duration: u64,
    pub samples: Vec<Mp4SampleOffset>,
    pub tkhd: crate::TkhdBox,
    pub mdia: crate::MdiaBox,
}

impl Mp4Track {
    fn new(trak: crate::TrakBox, offsets: &mut BTreeSet<u64>) -> Result<Mp4Track, BoxError> {
        let default_sample_duration = 1024;
        let mut total_duration = 0;
        let mut samples = Vec::with_capacity(trak.mdia.minf.stbl.stsz.sample_count as _);
        let stco = &trak.mdia.minf.stbl.stco;
        let co64 = &trak.mdia.minf.stbl.co64;

        let mb_iter1 = stco.clone().map(IntoIterator::into_iter);
        let mb_iter2 = co64.clone().map(IntoIterator::into_iter);

        if let Some(stco) = co64.as_ref().map(IntoIterator::into_iter) {
            offsets.extend(stco);
        }

        if let Some(stco) = stco.as_ref().map(IntoIterator::into_iter) {
            offsets.extend(stco);
        }

        let chunk_iter = chunk_iter(
            trak.mdia.minf.stbl.stsc.entries.clone().into_iter(),
            mb_iter1
                .into_iter()
                .flatten()
                .chain(mb_iter2.into_iter().flatten()),
        );

        let mut sample_chunk_iter = run_len_iter(chunk_iter);

        let sync_iter_peek = trak
            .mdia
            .minf
            .stbl
            .stss
            .as_ref()
            .map(|x| x.entries.iter().copied().peekable());

        let mut sync_iter =
            (1..=trak.mdia.minf.stbl.stsz.sample_count).scan(sync_iter_peek, |iter, idx| {
                let iter = iter.as_mut()?;

                Some(if idx == iter.peek().copied().unwrap_or(u32::MAX) {
                    iter.next();
                    true
                } else {
                    false
                })
            });

        let mut ts_deltas =
            run_len_iter(trak.mdia.minf.stbl.stts.entries.clone().into_iter().chain(
                std::iter::once(SttsEntry {
                    sample_count: u32::MAX,
                    sample_delta: default_sample_duration,
                }),
            ))
            .scan(0u64, |s, delta| {
                let out = *s;
                *s += delta as u64;
                Some((out, delta))
            });

        let mut rend_offset_iter = run_len_iter(
            trak.mdia
                .minf
                .stbl
                .ctts
                .clone()
                .into_iter()
                .flat_map(|x| x.entries.into_iter()),
        );

        let mut sample_offset = 0;
        let mut curr_chunk_index = 0;
        let mut prev_size = 0;

        for sample_idx in 0..trak.mdia.minf.stbl.stsz.sample_count as usize {
            let (start_time, duration) = ts_deltas.next().unwrap();
            let chunk = sample_chunk_iter.next().unwrap();
            let size = *trak
                .mdia
                .minf
                .stbl
                .stsz
                .sample_sizes
                .get(sample_idx)
                .unwrap_or(&trak.mdia.minf.stbl.stsz.sample_size);

            if curr_chunk_index != chunk.index {
                curr_chunk_index = chunk.index;
                sample_offset = 0;
            } else {
                sample_offset += prev_size;
            }

            prev_size = size;
            total_duration = start_time + duration as u64;
            samples.push(Mp4SampleOffset {
                chunk_id: chunk.index,
                offset: chunk.offset + sample_offset as u64,
                size,
                duration,
                start_time,
                rendering_offset: rend_offset_iter.next().unwrap_or(0),
                is_sync: sync_iter.next().unwrap_or(true),
            })
        }

        Ok(Self {
            track_id: trak.tkhd.track_id,
            tkhd: trak.tkhd,
            mdia: trak.mdia,
            samples,
            duration: total_duration,
        })
    }

    #[inline]
    pub fn track_type(&self) -> TrackType {
        TrackType::from(&self.mdia.hdlr.handler_type)
    }

    pub(crate) fn add_traf(
        &mut self,
        base_moof_offset: u64,
        chunk_index: u32,
        traf: crate::TrafBox,
        offsets: &mut BTreeSet<u64>,
    ) {
        let base_data_offset = traf.tfhd.base_data_offset.unwrap_or(base_moof_offset);
        offsets.insert(base_data_offset);

        let default_sample_size = traf.tfhd.default_sample_size.unwrap_or(0);
        let default_sample_duration = traf.tfhd.default_sample_duration.unwrap_or(0);
        let base_start_time = traf
            .tfdt
            .map(|x| x.base_media_decode_time)
            .or_else(|| {
                self.samples
                    .last()
                    .map(|x| x.start_time + x.duration as u64)
            })
            .unwrap_or(0);

        let Some(trun) = traf.trun else {
            return;
        };

        let mut sample_offset = 0u64;
        let mut start_time_offset = 0u64;
        for sample_idx in 0..trun.sample_count as usize {
            let size = trun
                .sample_sizes
                .get(sample_idx)
                .copied()
                .unwrap_or(default_sample_size);

            let duration = trun
                .sample_durations
                .get(sample_idx)
                .copied()
                .unwrap_or(default_sample_duration);

            let rendering_offset = trun.sample_cts.get(sample_idx).copied().unwrap_or(0) as i32;

            self.samples.push(Mp4SampleOffset {
                chunk_id: chunk_index,
                offset: (base_data_offset as i64
                    + trun.data_offset.map(|x| x as i64).unwrap_or(0)
                    + sample_offset as i64) as u64,
                size,
                duration,
                start_time: base_start_time + start_time_offset,
                rendering_offset,
                is_sync: sample_idx == 0,
            });

            sample_offset += size as u64;
            start_time_offset += duration as u64;
        }
    }
}

trait RunLenghtItem {
    type Value: Clone;

    fn count(&self) -> usize;
    fn value(&self) -> Self::Value;
}

impl<T: Clone> RunLenghtItem for (usize, T) {
    type Value = T;

    fn count(&self) -> usize {
        self.0
    }
    fn value(&self) -> Self::Value {
        self.1.clone()
    }
}

impl RunLenghtItem for CttsEntry {
    type Value = i32;

    fn count(&self) -> usize {
        self.sample_count as _
    }

    fn value(&self) -> Self::Value {
        self.sample_offset
    }
}

impl RunLenghtItem for SttsEntry {
    type Value = u32;

    fn count(&self) -> usize {
        self.sample_count as _
    }

    fn value(&self) -> Self::Value {
        self.sample_delta
    }
}

fn run_len_iter<E: RunLenghtItem, I: IntoIterator<Item = E>>(
    iter: I,
) -> impl Iterator<Item = E::Value> {
    let mut iter = iter.into_iter();
    let mut value = None::<E::Value>;
    let mut repeat = 0;
    std::iter::from_fn(move || loop {
        if let Some(val) = &value {
            if repeat > 0 {
                repeat -= 1;
                return Some(val.clone());
            } else {
                value = None;
            }
        }

        let x = iter.next()?;
        value = Some(x.value());
        repeat = x.count();
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk {
    pub index: u32,
    pub offset: u64,
    pub samples_per_chunk: u32,
    pub sample_description_index: u32,
}

impl RunLenghtItem for Chunk {
    type Value = Chunk;

    fn count(&self) -> usize {
        self.samples_per_chunk as _
    }

    fn value(&self) -> Self::Value {
        *self
    }
}

fn chunk_iter(
    mut stsc: impl Iterator<Item = StscEntry>,
    stco: impl Iterator<Item = u64>,
) -> impl Iterator<Item = Chunk> {
    let mut prev = stsc.next().unwrap_or(StscEntry {
        first_chunk: 1,
        samples_per_chunk: u32::MAX,
        sample_description_index: 1,
        first_sample: 1,
    });
    let mut curr = stsc.next();

    stco.enumerate().map(move |(idx, offset)| {
        if let Some(c) = &curr {
            if idx + 1 >= c.first_chunk as usize {
                prev = *c;
                curr = stsc.next();
            }
        }

        Chunk {
            index: idx as _,
            offset,
            samples_per_chunk: prev.samples_per_chunk,
            sample_description_index: prev.sample_description_index,
        }
    })
}
