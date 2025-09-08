use bytes::{BufMut, Bytes, BytesMut};
use flowly::Fourcc;
use std::collections::BTreeSet;

use crate::ctts::CttsEntry;
use crate::error::Error;
use crate::stsc::StscEntry;
use crate::stts::SttsEntry;
use crate::{BoxType, TrackType};

#[derive(Clone)]
pub struct Mp4SampleOffset {
    pub offset: u64,
    pub size: u32,
    pub duration: u32,
    pub start_time: u64,
    pub rendering_offset: i32,
    pub is_sync: bool,
    pub chunk_id: u32,
}

#[derive(Clone)]
pub struct Mp4Track {
    pub track_id: u32,
    pub duration: u64,
    pub samples: Vec<Mp4SampleOffset>,
    pub tkhd: crate::TkhdBox,
    pub mdia: crate::MdiaBox,
}

impl Mp4Track {
    pub fn new(trak: crate::TrakBox, offsets: &mut BTreeSet<u64>) -> Result<Mp4Track, Error> {
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

    #[inline]
    pub fn codec(&self) -> Fourcc {
        if self.mdia.minf.stbl.stsd.avc1.is_some() {
            Fourcc::VIDEO_AVC
        } else if self.mdia.minf.stbl.stsd.hev1.is_some() {
            Fourcc::VIDEO_HEVC
        } else if self.mdia.minf.stbl.stsd.vp09.is_some() {
            Fourcc::VIDEO_VP9
        } else if self.mdia.minf.stbl.stsd.mp4a.is_some() {
            Fourcc::AUDIO_AAC
        } else if self.mdia.minf.stbl.stsd.tx3g.is_some() {
            Fourcc::from_static("TTXT")
        } else {
            Default::default()
        }
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

    pub fn sequence_parameter_set(&self) -> Result<&[u8], Error> {
        if let Some(ref avc1) = self.mdia.minf.stbl.stsd.avc1 {
            match avc1.avcc.sequence_parameter_sets.first() {
                Some(nal) => Ok(nal.bytes.as_ref()),
                None => Err(Error::EntryInStblNotFound(
                    self.track_id,
                    BoxType::AvcCBox,
                    0,
                )),
            }
        } else {
            Err(Error::BoxInStblNotFound(self.track_id, BoxType::Avc1Box))
        }
    }

    pub fn picture_parameter_set(&self) -> Result<&[u8], Error> {
        if let Some(ref avc1) = self.mdia.minf.stbl.stsd.avc1 {
            match avc1.avcc.picture_parameter_sets.first() {
                Some(nal) => Ok(nal.bytes.as_ref()),
                None => Err(Error::EntryInStblNotFound(
                    self.track_id,
                    BoxType::AvcCBox,
                    0,
                )),
            }
        } else {
            Err(Error::BoxInStblNotFound(self.track_id, BoxType::Avc1Box))
        }
    }

    pub fn decode_params(&self) -> Option<Bytes> {
        match self.codec() {
            Fourcc::VIDEO_AVC => {
                let mut buf = BytesMut::new();

                let sps = self.sequence_parameter_set().unwrap();
                buf.put_u32(sps.len() as u32 + 4);
                buf.put_slice(&[0, 0, 0, 1]);
                buf.put_slice(sps);

                let pps = self.picture_parameter_set().unwrap();
                buf.put_u32(pps.len() as u32 + 4);
                buf.put_slice(&[0, 0, 0, 1]);
                buf.put_slice(pps);

                Some(buf.freeze())
            }

            Fourcc::VIDEO_HEVC => {
                let mut buf = BytesMut::new();
                let x = self.mdia.minf.stbl.stsd.hev1.as_ref().unwrap();
                for arr in &x.hvcc.arrays {
                    for nalu in &arr.nalus {
                        buf.put_u32(nalu.data.len() as u32 + 4);
                        buf.put_slice(&[0, 0, 0, 1]);
                        buf.put_slice(&nalu.data);
                    }
                }
                Some(buf.freeze())
            }

            _ => None,
        }
    }

    #[inline]
    pub fn timescale(&self) -> u32 {
        self.mdia.mdhd.timescale
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
