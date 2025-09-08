use std::sync::Arc;

use bytes::Bytes;
use flowly::{DataFrame, EncodedFrame, Fourcc, Frame, FrameFlags, FrameSource};

#[derive(Clone, Default, PartialEq)]
pub struct Mp4FrameSource<S> {
    pub original: S,
    pub params: Vec<Bytes>,
    pub codec: Fourcc,
    pub width: u16,
    pub height: u16,
}

impl<S: FrameSource> FrameSource for Mp4FrameSource<S> {
    type Source = S;

    fn source(&self) -> &Self::Source {
        &self.original
    }
}

#[derive(Clone)]
pub struct Mp4Frame<S> {
    source: Arc<Mp4FrameSource<S>>,
    timestamp: u64,
    offset: i32,
    data: Bytes,
    flags: FrameFlags,
}

impl<S> Mp4Frame<S> {
    pub fn new(
        source: Arc<Mp4FrameSource<S>>,
        timestamp: u64,
        offset: i32,
        data: Bytes,
        flags: FrameFlags,
    ) -> Self {
        Self {
            source,
            timestamp,
            offset,
            data,
            flags,
        }
    }
}

impl<S: FrameSource> DataFrame for Mp4Frame<S> {
    type Source = Arc<Mp4FrameSource<S>>;
    type Chunk = Bytes;

    fn source(&self) -> &Self::Source {
        &self.source
    }

    fn chunks(&self) -> impl Send + Iterator<Item = <Self::Chunk as flowly::MemBlock>::Ref<'_>> {
        std::iter::once(&self.data)
    }

    fn into_chunks(self) -> impl Send + Iterator<Item = Self::Chunk> {
        std::iter::once(self.data)
    }
}

impl<S: FrameSource> Frame for Mp4Frame<S> {
    fn timestamp(&self) -> u64 {
        self.timestamp
    }

    fn codec(&self) -> flowly::Fourcc {
        self.source.codec
    }

    fn flags(&self) -> flowly::FrameFlags {
        self.flags
    }
}

impl<S: FrameSource> EncodedFrame for Mp4Frame<S> {
    type Param = Bytes;

    fn pts(&self) -> i64 {
        self.timestamp as i64 + self.offset as i64
    }

    fn params(&self) -> impl Iterator<Item = &Self::Param> {
        self.source.params.iter()
    }
}
