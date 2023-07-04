use crate::header::{Codec, StreamInfo};
use crate::read::Reader;
use std::io::{Read, Write};

pub(crate) mod error;
mod vorbis;

pub(crate) fn encode<R: Read, W: Write>(
    codec: Codec,
    info: &StreamInfo,
    source: &mut Reader<R>,
    sink: W,
) -> Result<(), error::EncodeError> {
    match codec {
        Codec::Vorbis => vorbis::encode(
            u32::from(info.size) as usize,
            info.sample_rate,
            info.channels,
            source,
            sink,
        )?,
        _ => todo!(),
    }

    Ok(())
}
