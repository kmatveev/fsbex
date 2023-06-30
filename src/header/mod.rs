use crate::read::Reader;
pub(crate) mod error;
use bilge::prelude::*;
use error::{
    ChunkError, ChunkErrorKind, HeaderError, HeaderErrorKind, NameError, NameErrorKind,
    StreamError, StreamErrorKind,
};
use std::{
    ffi::CStr,
    io::Read,
    iter::zip,
    num::{NonZeroU32, NonZeroU8},
};

#[derive(Debug)]
pub(crate) struct Header {
    num_streams: NonZeroU32,
    codec: Codec,
    stream_info: Box<[StreamInfo]>,
}

impl Header {
    pub(crate) fn parse<R: Read>(reader: &mut Reader<R>) -> Result<Self, HeaderError> {
        match reader.take() {
            Ok(data) if data == FSB5_MAGIC => Ok(()),
            Err(e) => Err(HeaderError::new_with_source(HeaderErrorKind::Magic, e)),
            _ => Err(HeaderError::new(HeaderErrorKind::Magic)),
        }?;

        let version = reader
            .le_u32()
            .map_err(HeaderError::factory(HeaderErrorKind::Version))?
            .try_into()?;

        let num_streams: NonZeroU32 = reader
            .le_u32()
            .map_err(HeaderError::factory(HeaderErrorKind::StreamCount))?
            .try_into()
            .map_err(|_| HeaderError::new(HeaderErrorKind::ZeroStreams))?;

        let stream_headers_size = reader
            .le_u32()
            .map_err(HeaderError::factory(HeaderErrorKind::StreamHeadersSize))?;

        let name_table_size = reader
            .le_u32()
            .map_err(HeaderError::factory(HeaderErrorKind::NameTableSize))?;

        let total_stream_size = reader
            .le_u32()
            .map_err(HeaderError::factory(HeaderErrorKind::TotalStreamSize))?
            .try_into()
            .map_err(|_| HeaderError::new(HeaderErrorKind::ZeroTotalStreamSize))?;

        let codec = reader
            .le_u32()
            .map_err(HeaderError::factory(HeaderErrorKind::Codec))?
            .try_into()?;

        let base_header_size = match version {
            Version::V0 => 64,
            Version::V1 => 60,
        };

        reader
            .advance_to(base_header_size)
            .map_err(HeaderError::factory(HeaderErrorKind::Metadata))?;

        let mut stream_info = parse_stream_headers(reader, num_streams, total_stream_size)?;

        let header_size = base_header_size + stream_headers_size as usize;

        reader.advance_to(header_size).map_err(HeaderError::factory(
            HeaderErrorKind::WrongHeaderSize {
                expected: header_size,
                actual: reader.position(),
            },
        ))?;

        if name_table_size != 0 {
            let mut name_offsets = Vec::with_capacity(u32::from(num_streams) as usize);

            for index in 0..num_streams.into() {
                let offset = reader
                    .le_u32()
                    .map_err(NameError::read_factory(index, NameErrorKind::NameOffset))?;

                name_offsets.push(offset);
            }
            name_offsets.push(name_table_size);

            read_stream_names(reader, &name_offsets, &mut stream_info)?;
        }

        Ok(Self {
            num_streams,
            codec,
            stream_info: stream_info.into_boxed_slice(),
        })
    }
}

const FSB5_MAGIC: [u8; 4] = *b"FSB5";

enum Version {
    V0,
    V1,
}

impl TryFrom<u32> for Version {
    type Error = HeaderError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::V0),
            1 => Ok(Self::V1),
            version => Err(HeaderError::new(HeaderErrorKind::UnknownVersion { version })),
        }
    }
}

#[derive(Debug)]
enum Codec {
    Pcm8,
    Pcm16,
    Pcm24,
    Pcm32,
    PcmFloat,
    GcAdpcm,
    ImaAdpcm,
    Vag,
    HeVag,
    Xma,
    Mpeg,
    Celt,
    Atrac9,
    Xwma,
    Vorbis,
    FAdpcm,
    Opus,
}

impl TryFrom<u32> for Codec {
    type Error = HeaderError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Pcm8),
            2 => Ok(Self::Pcm16),
            3 => Ok(Self::Pcm24),
            4 => Ok(Self::Pcm32),
            5 => Ok(Self::PcmFloat),
            6 => Ok(Self::GcAdpcm),
            7 => Ok(Self::ImaAdpcm),
            8 => Ok(Self::Vag),
            9 => Ok(Self::HeVag),
            10 => Ok(Self::Xma),
            11 => Ok(Self::Mpeg),
            12 => Ok(Self::Celt),
            13 => Ok(Self::Atrac9),
            14 => Ok(Self::Xwma),
            15 => Ok(Self::Vorbis),
            16 => Ok(Self::FAdpcm),
            17 => Ok(Self::Opus),
            flag => Err(HeaderError::new(HeaderErrorKind::UnknownCodec { flag })),
        }
    }
}

fn parse_stream_headers<R: Read>(
    reader: &mut Reader<R>,
    num_streams: NonZeroU32,
    total_stream_size: NonZeroU32,
) -> Result<Vec<StreamInfo>, HeaderError> {
    let num_streams_usize = u32::from(num_streams) as usize;

    let mut stream_headers = Vec::with_capacity(num_streams_usize);
    let mut stream_offsets = Vec::with_capacity(num_streams_usize + 1);

    for index in 0..num_streams.into() {
        let mut stream_header = match reader.le_u64() {
            Ok(n) => RawStreamHeader::from(n).parse(index),
            Err(e) => Err(StreamError::new_with_source(index, StreamErrorKind::StreamInfo, e)),
        }?;

        if stream_header.has_chunks {
            parse_stream_chunks(reader, &mut stream_header)
                .map_err(|e| e.into_stream_err(index))?;
        }

        stream_offsets.push(stream_header.data_offset);
        stream_headers.push(stream_header);
    }
    stream_offsets.push(total_stream_size.into());

    let mut stream_info = Vec::with_capacity(num_streams_usize);

    for ((size, header), index) in zip(
        stream_offsets.windows(2).map(|window| window[1] - window[0]),
        stream_headers,
    )
    .zip(0..)
    {
        stream_info.push(
            header.with_stream_size(
                size.try_into()
                    .map_err(|_| HeaderError::new(HeaderErrorKind::ZeroStreamSize { index }))?,
            ),
        );
    }

    Ok(stream_info)
}

#[bitsize(64)]
#[derive(FromBits)]
struct RawStreamHeader {
    has_chunks: bool,
    sample_rate: u4,
    channels: u2,
    data_offset: u27,
    num_samples: u30,
}

#[derive(Debug, PartialEq)]
struct StreamHeader {
    has_chunks: bool,
    sample_rate: NonZeroU32,
    channels: NonZeroU8,
    data_offset: u32,
    num_samples: NonZeroU32,
    stream_loop: Option<Loop>,
    dsp_coeffs: Option<Box<[i16]>>,
}

impl RawStreamHeader {
    fn parse(self, stream_index: u32) -> Result<StreamHeader, StreamError> {
        let sample_rate = match self.sample_rate().value() {
            0 => Ok(4000),
            1 => Ok(8000),
            2 => Ok(11000),
            3 => Ok(11025),
            4 => Ok(16000),
            5 => Ok(22050),
            6 => Ok(24000),
            7 => Ok(32000),
            8 => Ok(44100),
            9 => Ok(48000),
            10 => Ok(96000),
            flag => Err(StreamError::new(
                stream_index,
                StreamErrorKind::UnknownSampleRate { flag },
            )),
        }?
        .try_into()
        .unwrap();

        let channels = match self.channels().value() {
            0 => 1,
            1 => 2,
            2 => 6,
            3 => 8,
            _ => unreachable!(),
        }
        .try_into()
        .unwrap();

        let num_samples = self
            .num_samples()
            .value()
            .try_into()
            .map_err(|_| StreamError::new(stream_index, StreamErrorKind::ZeroSamples))?;

        Ok(StreamHeader {
            has_chunks: self.has_chunks(),
            sample_rate,
            channels,
            data_offset: self.data_offset().value() * 32,
            num_samples,
            stream_loop: None,
            dsp_coeffs: None,
        })
    }
}

fn parse_stream_chunks<R: Read>(
    reader: &mut Reader<R>,
    stream: &mut StreamHeader,
) -> Result<(), ChunkError> {
    use crate::header::Loop;
    #[allow(clippy::enum_glob_use)]
    use StreamChunkKind::*;

    for index in 0.. {
        let chunk = match reader.le_u32() {
            Ok(n) => RawStreamChunk::from(n).parse(index),
            Err(e) => Err(ChunkError::new_with_source(index, ChunkErrorKind::Flag, e)),
        }?;

        let start_position = reader.position();

        match chunk.kind {
            Channels => {
                stream.channels = reader
                    .u8()
                    .map_err(ChunkError::factory(index, ChunkErrorKind::ChannelCount))?
                    .try_into()
                    .map_err(|_| ChunkError::new(index, ChunkErrorKind::ZeroChannels))?;
            }
            SampleRate => {
                stream.sample_rate = reader
                    .le_u32()
                    .map_err(ChunkError::factory(index, ChunkErrorKind::SampleRate))?
                    .try_into()
                    .map_err(|_| ChunkError::new(index, ChunkErrorKind::ZeroSampleRate))?;
            }
            Loop => {
                let start = reader
                    .le_u32()
                    .map_err(ChunkError::factory(index, ChunkErrorKind::LoopStart))?;

                let end = reader
                    .le_u32()
                    .map_err(ChunkError::factory(index, ChunkErrorKind::LoopEnd))?;

                stream.stream_loop = Some(Loop::parse(index, start, end)?);
            }
            DspCoefficients => {
                let channels = u8::from(stream.channels);

                let mut dsp_coeffs = Vec::with_capacity(channels as usize);

                for _ in 0..channels {
                    let mut coeff = 0;

                    for _ in 0..16 {
                        coeff += reader
                            .be_i16()
                            .map_err(ChunkError::factory(index, ChunkErrorKind::DspCoefficients))?;
                    }

                    reader
                        .skip(14)
                        .map_err(ChunkError::factory(index, ChunkErrorKind::DspCoefficients))?;

                    dsp_coeffs.push(coeff);
                }

                stream.dsp_coeffs = Some(dsp_coeffs.into_boxed_slice());
            }
            VorbisIntraLayers => {
                let layers = reader
                    .le_u32()
                    .map_err(ChunkError::factory(index, ChunkErrorKind::VorbisLayerCount))?;

                let layers: u8 = layers.try_into().map_err(|_| {
                    ChunkError::new(index, ChunkErrorKind::TooManyVorbisLayers { layers })
                })?;

                stream.channels = (u8::from(stream.channels) * layers)
                    .try_into()
                    .map_err(|_| ChunkError::new(index, ChunkErrorKind::ZeroVorbisLayers))?;
            }
            _ => {}
        }

        reader
            .advance_to(start_position + chunk.size as usize)
            .map_err(ChunkError::factory(
                index,
                ChunkErrorKind::WrongChunkSize {
                    expected: chunk.size,
                    actual: reader.position() - start_position,
                },
            ))?;

        if !chunk.more_chunks {
            break;
        }
    }

    Ok(())
}

#[bitsize(32)]
#[derive(FromBits)]
struct RawStreamChunk {
    more_chunks: bool,
    size: u24,
    kind: u7,
}

struct StreamChunk {
    more_chunks: bool,
    size: u32,
    kind: StreamChunkKind,
}

enum StreamChunkKind {
    Channels,
    SampleRate,
    Loop,
    Comment,
    XmaSeekTable,
    DspCoefficients,
    Atrac9Config,
    XwmaConfig,
    VorbisSeekTable,
    PeakVolume,
    VorbisIntraLayers,
    OpusDataSize,
}

impl RawStreamChunk {
    fn parse(self, chunk_index: u32) -> Result<StreamChunk, ChunkError> {
        #[allow(clippy::enum_glob_use)]
        use StreamChunkKind::*;

        let kind = match self.kind().value() {
            1 => Ok(Channels),
            2 => Ok(SampleRate),
            3 => Ok(Loop),
            4 => Ok(Comment),
            6 => Ok(XmaSeekTable),
            7 => Ok(DspCoefficients),
            9 => Ok(Atrac9Config),
            10 => Ok(XwmaConfig),
            11 => Ok(VorbisSeekTable),
            13 => Ok(PeakVolume),
            14 => Ok(VorbisIntraLayers),
            15 => Ok(OpusDataSize),
            flag => Err(ChunkError::new(chunk_index, ChunkErrorKind::UnknownType { flag })),
        }?;

        Ok(StreamChunk {
            more_chunks: self.more_chunks(),
            size: self.size().value(),
            kind,
        })
    }
}

#[derive(Debug, PartialEq)]
struct Loop {
    start: u32,
    len: NonZeroU32,
}

impl Loop {
    fn parse(index: u32, start: u32, end: u32) -> Result<Self, ChunkError> {
        let len = NonZeroU32::new(end - start)
            .ok_or_else(|| ChunkError::new(index, ChunkErrorKind::ZeroLengthLoop))?;

        Ok(Self { start, len })
    }
}

#[derive(Debug)]
struct StreamInfo {
    sample_rate: NonZeroU32,
    channels: NonZeroU8,
    num_samples: NonZeroU32,
    stream_loop: Option<Loop>,
    dsp_coeffs: Option<Box<[i16]>>,
    size: NonZeroU32,
    name: Option<Box<str>>,
}

impl StreamHeader {
    fn with_stream_size(self, size: NonZeroU32) -> StreamInfo {
        StreamInfo {
            sample_rate: self.sample_rate,
            channels: self.channels,
            num_samples: self.num_samples,
            stream_loop: self.stream_loop,
            dsp_coeffs: self.dsp_coeffs,
            size,
            name: None,
        }
    }
}

fn read_stream_names<R: Read>(
    reader: &mut Reader<R>,
    name_offsets: &[u32],
    stream_info: &mut [StreamInfo],
) -> Result<(), NameError> {
    for (name_len, index) in name_offsets.windows(2).map(|window| window[1] - window[0]).zip(0..) {
        let name_bytes = reader
            .take_len(name_len as usize)
            .map_err(NameError::read_factory(index, NameErrorKind::Name))?;

        let raw_name = CStr::from_bytes_with_nul(name_bytes.as_slice())
            .map_err(NameError::cstr_factory(index))?;

        let name = raw_name.to_str().map_err(NameError::utf8_factory(index))?.into();

        stream_info[index as usize].name = Some(name);
    }

    Ok(())
}

#[cfg(test)]
mod test {
    #[allow(clippy::enum_glob_use)]
    use super::error::{ChunkErrorKind::*, HeaderErrorKind::*, StreamErrorKind::*};
    use super::{Header, RawStreamChunk, RawStreamHeader, StreamHeader, FSB5_MAGIC};
    use crate::read::Reader;
    use std::num::{NonZeroU32, NonZeroU8};

    #[test]
    fn read_magic() {
        let mut reader;

        reader = Reader::new(b"".as_slice());
        assert!(Header::parse(&mut reader).is_err_and(|e| e.kind() == Magic));

        reader = Reader::new(b"abcd".as_slice());
        assert!(Header::parse(&mut reader).is_err_and(|e| e.kind() == Magic));

        reader = Reader::new(FSB5_MAGIC.as_slice());
        assert!(Header::parse(&mut reader).is_err_and(|e| e.kind() == Version));
    }

    #[test]
    fn read_version() {
        let mut reader;

        let data = b"FSB5\x00";
        reader = Reader::new(data.as_slice());
        assert!(Header::parse(&mut reader).is_err_and(|e| e.kind() == Version));

        let data = b"FSB5\xFF\x00\x00\x00";
        reader = Reader::new(data.as_slice());
        assert!(
            Header::parse(&mut reader).is_err_and(|e| e.kind() == UnknownVersion { version: 0xFF })
        );

        let data = b"FSB5\x00\x00\x00\x00";
        reader = Reader::new(data.as_slice());
        assert!(Header::parse(&mut reader).is_err_and(|e| e.kind() == StreamCount));
    }

    #[test]
    fn read_stream_count() {
        let mut reader;

        let data = b"FSB5\x01\x00\x00\x00\x00";
        reader = Reader::new(data.as_slice());
        assert!(Header::parse(&mut reader).is_err_and(|e| e.kind() == StreamCount));

        let data = b"FSB5\x01\x00\x00\x00\x00\x00\x00\x00";
        reader = Reader::new(data.as_slice());
        assert!(Header::parse(&mut reader).is_err_and(|e| e.kind() == ZeroStreams));

        let data = b"FSB5\x01\x00\x00\x00\x00\x00\xFF\xFF";
        reader = Reader::new(data.as_slice());
        assert!(Header::parse(&mut reader).is_err_and(|e| e.kind() == StreamHeadersSize));
    }

    #[test]
    fn read_stream_headers_size() {
        let mut reader;

        let data = b"FSB5\x01\x00\x00\x000000\x00";
        reader = Reader::new(data.as_slice());
        assert!(Header::parse(&mut reader).is_err_and(|e| e.kind() == StreamHeadersSize));

        let data = b"FSB5\x01\x00\x00\x0000000000";
        reader = Reader::new(data.as_slice());
        assert!(Header::parse(&mut reader).is_err_and(|e| e.kind() == NameTableSize));
    }

    #[test]
    fn read_name_table_size() {
        let mut reader;

        let data = b"FSB5\x01\x00\x00\x0000000000\x00";
        reader = Reader::new(data.as_slice());
        assert!(Header::parse(&mut reader).is_err_and(|e| e.kind() == NameTableSize));

        let data = b"FSB5\x01\x00\x00\x00000000000000";
        reader = Reader::new(data.as_slice());
        assert!(Header::parse(&mut reader).is_err_and(|e| e.kind() == TotalStreamSize));
    }

    #[test]
    fn read_stream_data_size() {
        let mut reader;

        let data = b"FSB5\x01\x00\x00\x00000000000000\x00";
        reader = Reader::new(data.as_slice());
        assert!(Header::parse(&mut reader).is_err_and(|e| e.kind() == TotalStreamSize));

        let data = b"FSB5\x01\x00\x00\x000000000000000000";
        reader = Reader::new(data.as_slice());
        assert!(Header::parse(&mut reader).is_err_and(|e| e.kind() == Codec));
    }

    #[test]
    fn read_codec() {
        let mut reader;

        let data = b"FSB5\x01\x00\x00\x000000000000000000\x00";
        reader = Reader::new(data.as_slice());
        assert!(Header::parse(&mut reader).is_err_and(|e| e.kind() == Codec));

        let data = b"FSB5\x01\x00\x00\x000000000000000000\x00\x00\x00\x00";
        reader = Reader::new(data.as_slice());
        assert!(Header::parse(&mut reader).is_err_and(|e| e.kind() == UnknownCodec { flag: 0 }));

        let data = b"FSB5\x01\x00\x00\x000000000000000000\x01\x00\x00\x00";
        reader = Reader::new(data.as_slice());
        assert!(Header::parse(&mut reader).is_err_and(|e| e.kind() == Metadata));
    }

    #[test]
    fn read_metadata() {
        const V0_HEADER_BASE: [u8; 28] = *b"FSB5\x00\x00\x00\x000000000000000000\x01\x00\x00\x00";
        const V1_HEADER_BASE: [u8; 28] = *b"FSB5\x01\x00\x00\x000000000000000000\x01\x00\x00\x00";

        let mut reader;

        let incomplete_data = b"FSB5\x01\x00\x00\x000000000000000000\x01\x00\x00\x00\x00";
        reader = Reader::new(incomplete_data.as_slice());
        assert!(Header::parse(&mut reader).is_err_and(|e| e.kind() == Metadata));

        let err_v1_data = {
            let mut buf = Vec::from(V1_HEADER_BASE);
            buf.append(&mut vec![0; 32]);
            buf
        };
        reader = Reader::new(&err_v1_data);
        assert!(Header::parse(&mut reader).is_err_and(|e| e.kind() == Metadata));

        let ok_v0_data = {
            let mut buf = Vec::from(V0_HEADER_BASE);
            buf.append(&mut vec![0; 32]);
            buf
        };
        reader = Reader::new(&ok_v0_data);
        assert!(Header::parse(&mut reader).is_err_and(|e| e.is_stream_err_kind(StreamInfo)));

        let ok_v1_data = {
            let mut buf = Vec::from(V1_HEADER_BASE);
            buf.append(&mut vec![0; 36]);
            buf
        };
        reader = Reader::new(&ok_v1_data);
        assert!(Header::parse(&mut reader).is_err_and(|e| e.is_stream_err_kind(StreamInfo)));
    }

    #[test]
    fn read_stream_info() {
        let data = b"FSB5\x01\x00\x00\x00\x01\x00\x00\x00000000000000\x01\x00\x00\x000000000000000000000000000000000000000000";
        let mut reader = Reader::new(data.as_slice());
        assert!(Header::parse(&mut reader).is_err_and(|e| e.is_stream_err_kind(StreamInfo)));
    }

    #[test]
    fn derived_stream_info_parsing_works() {
        #[allow(clippy::unusual_byte_groupings)]
        let data = 0b011010000101100111100000001011_111001101101001101000100110_11_1110_0;

        let mode = RawStreamHeader::from(data);

        let has_chunks = (data & 0x01) == 1;
        assert_eq!(mode.has_chunks(), has_chunks);

        let sample_rate_flag = (data >> 1) & 0x0F;
        assert_eq!(u64::from(mode.sample_rate()), sample_rate_flag);

        let channels_flag = (data >> 5) & 0x03;
        assert_eq!(u64::from(mode.channels()), channels_flag);

        let data_offset = ((data >> 7) & 0x07FF_FFFF) << 5;
        assert_eq!(u64::from(mode.data_offset()) * 32, data_offset);

        let num_samples = (data >> 34) & 0x3FFF_FFFF;
        assert_eq!(u64::from(mode.num_samples()), num_samples);
    }

    #[test]
    #[allow(clippy::unusual_byte_groupings)]
    fn parse_stream_info() {
        let data = 0b011010000101100111100000001011_111001101101001101000100110_11_1110_0;
        let mode = RawStreamHeader::from(data);
        assert!(mode
            .parse(0)
            .is_err_and(|e| e.kind() == UnknownSampleRate { flag: 0b1110 }));

        let data = 0b000000000000000000000000000000_111001101101001101000100110_11_0000_0;
        let mode = RawStreamHeader::from(data);
        assert!(mode.parse(0).is_err_and(|e| e.kind() == ZeroSamples));

        let data = 0b000000000000000000000000000001_000000000000000000000000001_01_1000_0;
        let mode = RawStreamHeader::from(data).parse(0).unwrap();
        assert_eq!(
            mode,
            StreamHeader {
                has_chunks: false,
                sample_rate: NonZeroU32::new(44100).unwrap(),
                channels: NonZeroU8::new(2).unwrap(),
                data_offset: 32,
                num_samples: NonZeroU32::new(1).unwrap(),
                stream_loop: None,
                dsp_coeffs: None,
            }
        );
    }

    #[test]
    fn derived_stream_chunk_parsing_works() {
        #[allow(clippy::unusual_byte_groupings)]
        let data = 0b0001101_100001101110000000011001_0;

        let flags = RawStreamChunk::from(data);

        let more_chunks = (data & 0x01) == 1;
        assert_eq!(flags.more_chunks(), more_chunks);

        let size = (data >> 1) & 0x00FF_FFFF;
        assert_eq!(u32::from(flags.size()), size);

        let kind = (data >> 25) & 0x7F;
        assert_eq!(u32::from(flags.kind()), kind);
    }

    #[test]
    fn parse_stream_chunk() {
        const DATA: &[u8; 72] = b"FSB5\x01\x00\x00\x00\x01\x00\x00\x00000000000000\x01\x00\x00\x00000000000000000000000000000000000000\x010000000";

        let mut reader;

        reader = Reader::new(DATA.as_slice());
        assert!(Header::parse(&mut reader).is_err_and(|e| e.is_chunk_err_kind(Flag)));

        #[allow(clippy::items_after_statements)]
        fn test_invalid_flag(kind: u8) {
            let flag = u32::from(kind).swap_bytes() << 1;
            assert!(RawStreamChunk::from(flag).parse(0).is_err());

            let full = {
                let mut buf = Vec::from(*DATA);
                buf.append(flag.to_le_bytes().to_vec().as_mut());
                buf
            };
            let mut reader = Reader::new(full.as_slice());
            assert!(Header::parse(&mut reader)
                .is_err_and(|e| e.is_chunk_err_kind(UnknownType { flag: kind })));
        }

        for flag in [0, 5, 8, 12] {
            test_invalid_flag(flag);
        }
        for flag in 16..128 {
            test_invalid_flag(flag);
        }
    }
}
