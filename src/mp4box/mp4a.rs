use byteorder::{BigEndian, WriteBytesExt};
use serde::Serialize;
use std::io::Write;

use crate::mp4box::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Mp4aBox {
    pub data_reference_index: u16,
    pub channelcount: u16,
    pub samplesize: u16,

    #[serde(with = "value_u32")]
    pub samplerate: FixedPointU16,
    pub esds: Option<EsdsBox>,
}

impl Default for Mp4aBox {
    fn default() -> Self {
        Self {
            data_reference_index: 0,
            channelcount: 2,
            samplesize: 16,
            samplerate: FixedPointU16::new(48000),
            esds: Some(EsdsBox::default()),
        }
    }
}

impl Mp4aBox {
    pub fn new(config: &AacConfig) -> Self {
        Self {
            data_reference_index: 1,
            channelcount: config.chan_conf as u16,
            samplesize: 16,
            samplerate: FixedPointU16::new(config.freq_index.freq() as u16),
            esds: Some(EsdsBox::new(config)),
        }
    }

    pub fn get_type(&self) -> BoxType {
        BoxType::Mp4aBox
    }

    pub fn get_size(&self) -> u64 {
        let mut size = HEADER_SIZE + 8 + 20;
        if let Some(ref esds) = self.esds {
            size += esds.box_size();
        }
        size
    }
}

impl Mp4Box for Mp4aBox {
    const TYPE: BoxType = BoxType::Mp4aBox;

    fn box_size(&self) -> u64 {
        self.get_size()
    }

    fn to_json(&self) -> Result<String, Error> {
        Ok(serde_json::to_string(&self).unwrap())
    }

    fn summary(&self) -> Result<String, Error> {
        let s = format!(
            "channel_count={} sample_size={} sample_rate={}",
            self.channelcount,
            self.samplesize,
            self.samplerate.value()
        );
        Ok(s)
    }
}

impl BlockReader for Mp4aBox {
    fn read_block<'a>(reader: &mut impl Reader<'a>) -> Result<Self, Error> {
        reader.get_u32(); // reserved
        reader.get_u16(); // reserved

        let data_reference_index = reader.get_u16();
        let version = reader.get_u16();

        reader.get_u16(); // reserved
        reader.get_u32(); // reserved

        let channelcount = reader.get_u16();
        let samplesize = reader.get_u16();

        reader.get_u32(); // pre-defined, reserved

        let samplerate = FixedPointU16::new_raw(reader.get_u32());

        if version == 1 {
            if reader.remaining() < 16 {
                return Err(Error::InvalidData("expected at least 16 bytes more"));
            }

            // Skip QTFF
            reader.get_u64();
            reader.get_u64();
        }

        Ok(Mp4aBox {
            data_reference_index,
            channelcount,
            samplesize,
            samplerate,
            esds: reader.try_find_box::<EsdsBox>()?,
        })
    }

    fn size_hint() -> usize {
        28
    }
}

impl<W: Write> WriteBox<&mut W> for Mp4aBox {
    fn write_box(&self, writer: &mut W) -> Result<u64, Error> {
        let size = self.box_size();
        BoxHeader::new(Self::TYPE, size).write(writer)?;

        writer.write_u32::<BigEndian>(0)?; // reserved
        writer.write_u16::<BigEndian>(0)?; // reserved
        writer.write_u16::<BigEndian>(self.data_reference_index)?;

        writer.write_u64::<BigEndian>(0)?; // reserved
        writer.write_u16::<BigEndian>(self.channelcount)?;
        writer.write_u16::<BigEndian>(self.samplesize)?;
        writer.write_u32::<BigEndian>(0)?; // reserved
        writer.write_u32::<BigEndian>(self.samplerate.raw_value())?;

        if let Some(ref esds) = self.esds {
            esds.write_box(writer)?;
        }

        Ok(size)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct EsdsBox {
    pub version: u8,
    pub flags: u32,
    pub es_desc: ESDescriptor,
}

impl EsdsBox {
    pub fn new(config: &AacConfig) -> Self {
        Self {
            version: 0,
            flags: 0,
            es_desc: ESDescriptor::new(config),
        }
    }
}

impl Mp4Box for EsdsBox {
    const TYPE: BoxType = BoxType::EsdsBox;

    fn box_size(&self) -> u64 {
        HEADER_SIZE
            + HEADER_EXT_SIZE
            + 1
            + size_of_length(ESDescriptor::desc_size()) as u64
            + ESDescriptor::desc_size() as u64
    }

    fn to_json(&self) -> Result<String, Error> {
        Ok(serde_json::to_string(&self).unwrap())
    }

    fn summary(&self) -> Result<String, Error> {
        Ok(String::new())
    }
}

impl BlockReader for EsdsBox {
    fn read_block<'a>(reader: &mut impl Reader<'a>) -> Result<Self, Error> {
        let (version, flags) = read_box_header_ext(reader);

        let mut es_desc = None;

        while let Some((desc_tag, desc_size)) = read_desc(reader) {
            match desc_tag {
                0x03 => {
                    es_desc = Some(ESDescriptor::read_block(&mut reader.take(desc_size as _)?)?);
                }
                _ => break,
            }
        }

        if es_desc.is_none() {
            return Err(Error::InvalidData("ESDescriptor not found"));
        }

        Ok(EsdsBox {
            version,
            flags,
            es_desc: es_desc.unwrap(),
        })
    }

    fn size_hint() -> usize {
        4 + ESDescriptor::size_hint()
    }
}

impl<W: Write> WriteBox<&mut W> for EsdsBox {
    fn write_box(&self, writer: &mut W) -> Result<u64, Error> {
        let size = self.box_size();
        BoxHeader::new(Self::TYPE, size).write(writer)?;

        write_box_header_ext(writer, self.version, self.flags)?;

        self.es_desc.write_desc(writer)?;

        Ok(size)
    }
}

trait Descriptor: Sized {
    fn desc_tag() -> u8;
    fn desc_size() -> u32;
}

trait WriteDesc<T>: Sized {
    fn write_desc(&self, _: T) -> Result<u32, Error>;
}

fn read_desc<'a, R: Reader<'a>>(reader: &mut R) -> Option<(u8, u32)> {
    let tag = reader.try_get_u8().ok()?;

    let mut size: u32 = 0;
    for _ in 0..4 {
        let b = reader.try_get_u8().ok()?;
        size = (size << 7) | (b & 0x7F) as u32;

        if b & 0x80 == 0 {
            break;
        }
    }

    Some((tag, size))
}

fn size_of_length(size: u32) -> u32 {
    match size {
        0x0..=0x7F => 1,
        0x80..=0x3FFF => 2,
        0x4000..=0x1FFFFF => 3,
        _ => 4,
    }
}

fn write_desc<W: Write>(writer: &mut W, tag: u8, size: u32) -> Result<u64, Error> {
    writer.write_u8(tag)?;

    if size as u64 > std::u32::MAX as u64 {
        return Err(Error::InvalidData("invalid descriptor length range"));
    }

    let nbytes = size_of_length(size);

    for i in 0..nbytes {
        let mut b = (size >> ((nbytes - i - 1) * 7)) as u8 & 0x7F;
        if i < nbytes - 1 {
            b |= 0x80;
        }
        writer.write_u8(b)?;
    }

    Ok(1 + nbytes as u64)
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct ESDescriptor {
    pub es_id: u16,

    pub dec_config: DecoderConfigDescriptor,
    pub sl_config: SLConfigDescriptor,
}

impl ESDescriptor {
    pub fn new(config: &AacConfig) -> Self {
        Self {
            es_id: 1,
            dec_config: DecoderConfigDescriptor::new(config),
            sl_config: SLConfigDescriptor::new(),
        }
    }
}

impl Descriptor for ESDescriptor {
    fn desc_tag() -> u8 {
        0x03
    }

    fn desc_size() -> u32 {
        3 + 1
            + size_of_length(DecoderConfigDescriptor::desc_size())
            + DecoderConfigDescriptor::desc_size()
            + 1
            + size_of_length(SLConfigDescriptor::desc_size())
            + SLConfigDescriptor::desc_size()
    }
}

impl BlockReader for ESDescriptor {
    fn read_block<'a>(reader: &mut impl Reader<'a>) -> Result<Self, Error> {
        let es_id = reader.get_u16();
        reader.get_u8(); // XXX flags must be 0

        let mut dec_config = None;
        let mut sl_config = None;

        while let Some((desc_tag, desc_size)) = read_desc(reader) {
            match desc_tag {
                0x04 => {
                    let mut rdr = reader.take(desc_size as _)?;
                    dec_config = Some(DecoderConfigDescriptor::read_block(&mut rdr)?);
                    rdr.skip(rdr.remaining());
                }
                0x06 => {
                    let mut rdr = reader.take(desc_size as _)?;
                    sl_config = Some(SLConfigDescriptor::read_block(&mut rdr)?);
                    rdr.skip(rdr.remaining());
                }
                _ => reader.skip(desc_size as _),
            }
        }

        Ok(ESDescriptor {
            es_id,
            dec_config: dec_config.unwrap_or_default(),
            sl_config: sl_config.unwrap_or_default(),
        })
    }

    fn size_hint() -> usize {
        3
    }
}

impl<W: Write> WriteDesc<&mut W> for ESDescriptor {
    fn write_desc(&self, writer: &mut W) -> Result<u32, Error> {
        let size = Self::desc_size();
        write_desc(writer, Self::desc_tag(), size)?;

        writer.write_u16::<BigEndian>(self.es_id)?;
        writer.write_u8(0)?;

        self.dec_config.write_desc(writer)?;
        self.sl_config.write_desc(writer)?;

        Ok(size)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct DecoderConfigDescriptor {
    pub object_type_indication: u8,
    pub stream_type: u8,
    pub up_stream: u8,
    pub buffer_size_db: u32,
    pub max_bitrate: u32,
    pub avg_bitrate: u32,

    pub dec_specific: DecoderSpecificDescriptor,
}

impl DecoderConfigDescriptor {
    pub fn new(config: &AacConfig) -> Self {
        Self {
            object_type_indication: 0x40, // XXX AAC
            stream_type: 0x05,            // XXX Audio
            up_stream: 0,
            buffer_size_db: 0,
            max_bitrate: config.bitrate, // XXX
            avg_bitrate: config.bitrate,
            dec_specific: DecoderSpecificDescriptor::new(config),
        }
    }
}

impl Descriptor for DecoderConfigDescriptor {
    fn desc_tag() -> u8 {
        0x04
    }

    fn desc_size() -> u32 {
        13 + 1
            + size_of_length(DecoderSpecificDescriptor::desc_size())
            + DecoderSpecificDescriptor::desc_size()
    }
}

impl BlockReader for DecoderConfigDescriptor {
    fn read_block<'a>(reader: &mut impl Reader<'a>) -> Result<Self, Error> {
        let object_type_indication = reader.get_u8();
        let byte_a = reader.get_u8();
        let stream_type = (byte_a & 0xFC) >> 2;
        let up_stream = byte_a & 0x02;
        let buffer_size_db = reader.get_u24();
        let max_bitrate = reader.get_u32();
        let avg_bitrate = reader.get_u32();

        let mut dec_specific = None;

        while let Some((desc_tag, desc_size)) = read_desc(reader) {
            match desc_tag {
                0x05 => {
                    let mut rdr = reader.take(desc_size as _)?;
                    let r = DecoderSpecificDescriptor::read_block(&mut rdr)?;
                    rdr.skip(rdr.remaining());
                    dec_specific = Some(r);
                }
                _ => reader.skip(desc_size as _),
            }
        }

        Ok(DecoderConfigDescriptor {
            object_type_indication,
            stream_type,
            up_stream,
            buffer_size_db,
            max_bitrate,
            avg_bitrate,
            dec_specific: dec_specific.unwrap_or_default(),
        })
    }

    fn size_hint() -> usize {
        13
    }
}

impl<W: Write> WriteDesc<&mut W> for DecoderConfigDescriptor {
    fn write_desc(&self, writer: &mut W) -> Result<u32, Error> {
        let size = Self::desc_size();
        write_desc(writer, Self::desc_tag(), size)?;

        writer.write_u8(self.object_type_indication)?;
        writer.write_u8((self.stream_type << 2) + (self.up_stream & 0x02) + 1)?; // 1 reserved
        writer.write_u24::<BigEndian>(self.buffer_size_db)?;
        writer.write_u32::<BigEndian>(self.max_bitrate)?;
        writer.write_u32::<BigEndian>(self.avg_bitrate)?;

        self.dec_specific.write_desc(writer)?;

        Ok(size)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct DecoderSpecificDescriptor {
    pub profile: u8,
    pub freq_index: u8,
    pub chan_conf: u8,
}

impl DecoderSpecificDescriptor {
    pub fn new(config: &AacConfig) -> Self {
        Self {
            profile: config.profile as u8,
            freq_index: config.freq_index as u8,
            chan_conf: config.chan_conf as u8,
        }
    }
}

impl Descriptor for DecoderSpecificDescriptor {
    fn desc_tag() -> u8 {
        0x05
    }

    fn desc_size() -> u32 {
        2
    }
}

fn get_audio_object_type(byte_a: u8, byte_b: u8) -> u8 {
    let mut profile = byte_a >> 3;
    if profile == 31 {
        profile = 32 + ((byte_a & 7) | (byte_b >> 5));
    }

    profile
}

fn get_chan_conf<'a, R: Reader<'a>>(
    reader: &mut R,
    byte_b: u8,
    freq_index: u8,
    extended_profile: bool,
) -> Result<u8, Error> {
    let chan_conf;
    if freq_index == 15 {
        // Skip the 24 bit sample rate
        let sample_rate = reader.try_get_u24()?;
        chan_conf = ((sample_rate >> 4) & 0x0F) as u8;
    } else if extended_profile {
        let byte_c = reader.try_get_u8()?;
        chan_conf = (byte_b & 1) | (byte_c & 0xE0);
    } else {
        chan_conf = (byte_b >> 3) & 0x0F;
    }

    Ok(chan_conf)
}

impl BlockReader for DecoderSpecificDescriptor {
    fn read_block<'a>(reader: &mut impl Reader<'a>) -> Result<Self, Error> {
        let byte_a = reader.get_u8();
        let byte_b = reader.get_u8();
        let profile = get_audio_object_type(byte_a, byte_b);

        let freq_index;
        let chan_conf;
        if profile > 31 {
            freq_index = (byte_b >> 1) & 0x0F;
            chan_conf = get_chan_conf(reader, byte_b, freq_index, true)?;
        } else {
            freq_index = ((byte_a & 0x07) << 1) + (byte_b >> 7);
            chan_conf = get_chan_conf(reader, byte_b, freq_index, false)?;
        }

        Ok(DecoderSpecificDescriptor {
            profile,
            freq_index,
            chan_conf,
        })
    }

    fn size_hint() -> usize {
        2
    }
}

impl<W: Write> WriteDesc<&mut W> for DecoderSpecificDescriptor {
    fn write_desc(&self, writer: &mut W) -> Result<u32, Error> {
        let size = Self::desc_size();
        write_desc(writer, Self::desc_tag(), size)?;

        writer.write_u8((self.profile << 3) + (self.freq_index >> 1))?;
        writer.write_u8((self.freq_index << 7) + (self.chan_conf << 3))?;

        Ok(size)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct SLConfigDescriptor {}

impl SLConfigDescriptor {
    pub fn new() -> Self {
        SLConfigDescriptor {}
    }
}

impl Descriptor for SLConfigDescriptor {
    fn desc_tag() -> u8 {
        0x06
    }

    fn desc_size() -> u32 {
        1
    }
}

impl BlockReader for SLConfigDescriptor {
    fn read_block<'a>(reader: &mut impl Reader<'a>) -> Result<Self, Error> {
        reader.get_u8(); // pre-defined

        Ok(SLConfigDescriptor {})
    }

    fn size_hint() -> usize {
        1
    }
}

impl<W: Write> WriteDesc<&mut W> for SLConfigDescriptor {
    fn write_desc(&self, writer: &mut W) -> Result<u32, Error> {
        let size = Self::desc_size();
        write_desc(writer, Self::desc_tag(), size)?;

        writer.write_u8(2)?; // pre-defined
        Ok(size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mp4box::BoxHeader;

    #[tokio::test]
    async fn test_mp4a() {
        let src_box = Mp4aBox {
            data_reference_index: 1,
            channelcount: 2,
            samplesize: 16,
            samplerate: FixedPointU16::new(48000),
            esds: Some(EsdsBox {
                version: 0,
                flags: 0,
                es_desc: ESDescriptor {
                    es_id: 2,
                    dec_config: DecoderConfigDescriptor {
                        object_type_indication: 0x40,
                        stream_type: 0x05,
                        up_stream: 0,
                        buffer_size_db: 0,
                        max_bitrate: 67695,
                        avg_bitrate: 67695,
                        dec_specific: DecoderSpecificDescriptor {
                            profile: 2,
                            freq_index: 3,
                            chan_conf: 1,
                        },
                    },
                    sl_config: SLConfigDescriptor::default(),
                },
            }),
        };
        let mut buf = Vec::new();
        src_box.write_box(&mut buf).unwrap();
        assert_eq!(buf.len(), src_box.box_size() as usize);

        let mut reader = buf.as_slice();
        let header = BoxHeader::read(&mut reader, &mut 0).await.unwrap().unwrap();
        assert_eq!(header.kind, BoxType::Mp4aBox);
        assert_eq!(src_box.box_size(), header.size);

        let dst_box = Mp4aBox::read_block(&mut reader).unwrap();
        assert_eq!(src_box, dst_box);
    }

    #[tokio::test]
    async fn test_mp4a_no_esds() {
        let src_box = Mp4aBox {
            data_reference_index: 1,
            channelcount: 2,
            samplesize: 16,
            samplerate: FixedPointU16::new(48000),
            esds: None,
        };
        let mut buf = Vec::new();
        src_box.write_box(&mut buf).unwrap();
        assert_eq!(buf.len(), src_box.box_size() as usize);

        let mut reader = buf.as_slice();
        let header = BoxHeader::read(&mut reader, &mut 0).await.unwrap().unwrap();
        assert_eq!(header.kind, BoxType::Mp4aBox);
        assert_eq!(src_box.box_size(), header.size);

        let dst_box = Mp4aBox::read_block(&mut reader).unwrap();
        assert_eq!(src_box, dst_box);
    }
}
