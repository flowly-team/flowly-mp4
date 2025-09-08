use serde::Serialize;

use crate::mp4box::meta::MetaBox;
use crate::mp4box::*;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct UdtaBox {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaBox>,
}

impl UdtaBox {
    pub fn get_type(&self) -> BoxType {
        BoxType::UdtaBox
    }

    pub fn get_size(&self) -> u64 {
        let mut size = HEADER_SIZE;
        if let Some(meta) = &self.meta {
            size += meta.box_size();
        }
        size
    }
}

impl Mp4Box for UdtaBox {
    const TYPE: BoxType = BoxType::UdtaBox;

    fn box_size(&self) -> u64 {
        self.get_size()
    }

    fn to_json(&self) -> Result<String, Error> {
        Ok(serde_json::to_string(&self).unwrap())
    }

    fn summary(&self) -> Result<String, Error> {
        Ok(String::new())
    }
}

impl BlockReader for UdtaBox {
    fn read_block<'a>(reader: &mut impl Reader<'a>) -> Result<Self, Error> {
        Ok(UdtaBox {
            meta: reader.try_find_box()?,
        })
    }

    fn size_hint() -> usize {
        0
    }
}

impl<W: Write> WriteBox<&mut W> for UdtaBox {
    fn write_box(&self, writer: &mut W) -> Result<u64, Error> {
        let size = self.box_size();
        BoxHeader::new(Self::TYPE, size).write(writer)?;

        if let Some(meta) = &self.meta {
            meta.write_box(writer)?;
        }
        Ok(size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mp4box::BoxHeader;

    #[tokio::test]
    async fn test_udta_empty() {
        let src_box = UdtaBox { meta: None };

        let mut buf = Vec::new();
        src_box.write_box(&mut buf).unwrap();
        assert_eq!(buf.len(), src_box.box_size() as usize);

        let mut reader = buf.as_slice();
        let header = BoxHeader::read(&mut reader, &mut 0).await.unwrap().unwrap();
        assert_eq!(header.kind, BoxType::UdtaBox);
        assert_eq!(header.size, src_box.box_size());

        let dst_box = UdtaBox::read_block(&mut reader).unwrap();
        assert_eq!(dst_box, src_box);
    }

    #[tokio::test]
    async fn test_udta() {
        let src_box = UdtaBox {
            meta: Some(MetaBox::default()),
        };

        let mut buf = Vec::new();
        src_box.write_box(&mut buf).unwrap();
        assert_eq!(buf.len(), src_box.box_size() as usize);

        let mut reader = buf.as_slice();
        let header = BoxHeader::read(&mut reader, &mut 0).await.unwrap().unwrap();
        assert_eq!(header.kind, BoxType::UdtaBox);
        assert_eq!(header.size, src_box.box_size());

        let dst_box = UdtaBox::read_block(&mut reader).unwrap();
        assert_eq!(dst_box, src_box);
    }
}
