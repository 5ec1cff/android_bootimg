use crate::utils::{Chunker, ReadExt, WriteExt};
use bzip2::read::BzDecoder;
use bzip2::write::BzEncoder;
use bzip2::Compression as BzCompression;
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression as GzCompression;
use lz4::block::CompressionMode;
use lz4::liblz4::BlockChecksum;
use lz4::{BlockMode, BlockSize, ContentChecksum, Decoder as LZ4FrameDecoder, Encoder as LZ4FrameEncoder, EncoderBuilder as LZ4FrameEncoderBuilder};
use lzma_rust2::{CheckType, LzmaOptions, LzmaReader, LzmaWriter, XzOptions, XzReader, XzWriter};
use std::cmp::min;
use std::io::{BufWriter, Read, Write};
use std::num::NonZeroU64;
use zopfli::{BlockType, GzipEncoder as ZopFliEncoder, Options as ZopfliOptions};

const GZIP1_MAGIC: &[u8] = b"\x1f\x8b";
const GZIP2_MAGIC: &[u8] = b"\x1f\x9e";
const LZOP_MAGIC: &[u8] = b"\x89LZO";
const XZ_MAGIC: &[u8] = b"\xfd7zXZ";
const BZIP_MAGIC: &[u8] = b"BZh";
const LZ4_LEG_MAGIC: &[u8] = b"\x02\x21\x4c\x18";
const LZ41_MAGIC: &[u8] = b"\x03\x21\x4c\x18";
const LZ42_MAGIC: &[u8] = b"\x04\x22\x4d\x18";

// https://github.com/topjohnwu/Magisk/blob/01cb75eaefbd14c2d10772ded3942660ebf0285f/native/src/boot/lib.rs#L25-L48
// https://github.com/topjohnwu/Magisk/blob/01cb75eaefbd14c2d10772ded3942660ebf0285f/native/src/boot/format.rs#L62
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum CompressFormat {
    UNKNOWN,
    GZIP,
    ZOPFLI,
    LZOP,
    XZ,
    LZMA,
    BZIP2,
    LZ4,
    LZ4_LEGACY,
    // LZ4_LG,
}

// https://github.com/topjohnwu/Magisk/blob/01cb75eaefbd14c2d10772ded3942660ebf0285f/native/src/boot/magiskboot.hpp#L21-L50
// https://github.com/topjohnwu/Magisk/blob/01cb75eaefbd14c2d10772ded3942660ebf0285f/native/src/boot/bootimg.cpp#L69

fn guess_lzma(data: &[u8]) -> bool {
    if data.len() <= 13 {
        return false;
    }

    if data[0] != b'\x5d' {
        return false;
    }

    let dict_size = u32::from_le_bytes(data[1..5].try_into().unwrap());

    if dict_size == 0 || (dict_size & (dict_size - 1)) != 0 {
        return false;
    }

    &data[5..13] == b"\xff\xff\xff\xff\xff\xff\xff\xff"
}

pub fn parse_compress_format(data: &[u8]) -> CompressFormat {
    if data.starts_with(GZIP1_MAGIC) || data.starts_with(GZIP2_MAGIC) {
        CompressFormat::GZIP
    } else if data.starts_with(LZOP_MAGIC) {
        CompressFormat::LZOP
    } else if data.starts_with(XZ_MAGIC) {
        CompressFormat::XZ
    } else if data.starts_with(BZIP_MAGIC) {
        CompressFormat::BZIP2
    } else if data.starts_with(LZ41_MAGIC) || data.starts_with(LZ42_MAGIC) {
        CompressFormat::LZ4
    } else if data.starts_with(LZ4_LEG_MAGIC) {
        CompressFormat::LZ4_LEGACY
    } else if guess_lzma(data) {
        CompressFormat::LZMA
    } else {
        CompressFormat::UNKNOWN
    }
}


pub trait WriteFinish<W: Write>: Write {
    fn finish(self: Box<Self>) -> std::io::Result<W>;
}

// Boilerplate for existing types

macro_rules! finish_impl {
    ($($t:ty),*) => {$(
        impl<W: Write> WriteFinish<W> for $t {
            fn finish(self: Box<Self>) -> std::io::Result<W> {
                Self::finish(*self)
            }
        }
    )*}
}

finish_impl!(GzEncoder<W>, BzEncoder<W>, XzWriter<W>, LzmaWriter<W>);

impl<W: Write> WriteFinish<W> for BufWriter<ZopFliEncoder<W>> {
    fn finish(self: Box<Self>) -> std::io::Result<W> {
        let inner = self.into_inner()?;
        ZopFliEncoder::finish(inner)
    }
}

impl<W: Write> WriteFinish<W> for LZ4FrameEncoder<W> {
    fn finish(self: Box<Self>) -> std::io::Result<W> {
        let (w, r) = Self::finish(*self);
        r?;
        Ok(w)
    }
}


// LZ4BlockArchive format
//
// len:  |   4   |          4            |           n           | ... |           4             |
// data: | magic | compressed block size | compressed block data | ... | total uncompressed size |

// LZ4BlockEncoder

const LZ4_BLOCK_SIZE: usize = 0x800000;
const LZ4HC_CLEVEL_MAX: i32 = 12;
const LZ4_MAGIC: u32 = 0x184c2102;

struct LZ4BlockEncoder<W: Write> {
    write: W,
    chunker: Chunker,
    out_buf: Box<[u8]>,
    total: u32,
    is_lg: bool,
}

impl<W: Write> LZ4BlockEncoder<W> {
    fn new(write: W, is_lg: bool) -> Self {
        let out_sz = lz4::block::compress_bound(LZ4_BLOCK_SIZE).unwrap_or(LZ4_BLOCK_SIZE);
        LZ4BlockEncoder {
            write,
            chunker: Chunker::new(LZ4_BLOCK_SIZE),
            // SAFETY: all bytes will be initialized before it is used
            out_buf: unsafe { Box::new_uninit_slice(out_sz).assume_init() },
            total: 0,
            is_lg,
        }
    }

    fn encode_block(write: &mut W, out_buf: &mut [u8], chunk: &[u8]) -> std::io::Result<()> {
        let compressed_size = lz4::block::compress_to_buffer(
            chunk,
            Some(CompressionMode::HIGHCOMPRESSION(LZ4HC_CLEVEL_MAX)),
            false,
            out_buf,
        )?;
        let block_size = compressed_size as u32;
        write.write_pod(&block_size)?;
        write.write_all(&out_buf[..compressed_size])
    }
}

// https://github.com/topjohnwu/Magisk/blob/0bbc7360519726f7e3b5004542c0131fa0c0c86f/native/src/base/misc.rs#L204-L260

impl<W: Write> Write for LZ4BlockEncoder<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn write_all(&mut self, mut buf: &[u8]) -> std::io::Result<()> {
        if self.total == 0 {
            // Write header
            self.write.write_pod(&LZ4_MAGIC)?;
        }

        self.total += buf.len() as u32;
        while !buf.is_empty() {
            let (b, chunk) = self.chunker.add_data(buf);
            buf = b;
            if let Some(chunk) = chunk {
                Self::encode_block(&mut self.write, &mut self.out_buf, chunk)?;
            }
        }
        Ok(())
    }
}

impl<W: Write> WriteFinish<W> for LZ4BlockEncoder<W> {
    fn finish(mut self: Box<Self>) -> std::io::Result<W> {
        let chunk = self.chunker.get_available();
        if !chunk.is_empty() {
            Self::encode_block(&mut self.write, &mut self.out_buf, chunk)?;
        }
        if self.is_lg {
            self.write.write_pod(&self.total)?;
        }
        Ok(self.write)
    }
}

// LZ4BlockDecoder

struct LZ4BlockDecoder<R: Read> {
    read: R,
    in_buf: Box<[u8]>,
    out_buf: Box<[u8]>,
    out_len: usize,
    out_pos: usize,
}

impl<R: Read> LZ4BlockDecoder<R> {
    fn new(read: R) -> Self {
        let compressed_sz = lz4::block::compress_bound(LZ4_BLOCK_SIZE).unwrap_or(LZ4_BLOCK_SIZE);
        Self {
            read,
            in_buf: unsafe { Box::new_uninit_slice(compressed_sz).assume_init() },
            out_buf: unsafe { Box::new_uninit_slice(LZ4_BLOCK_SIZE).assume_init() },
            out_len: 0,
            out_pos: 0,
        }
    }
}

impl<R: Read> Read for LZ4BlockDecoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.out_pos == self.out_len {
            let mut block_size: u32 = 0;
            if let Err(e) = self.read.read_pod(&mut block_size) {
                return if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    Ok(0)
                } else {
                    Err(e)
                };
            }
            if block_size == LZ4_MAGIC {
                self.read.read_pod(&mut block_size)?;
            }

            let block_size = block_size as usize;

            if block_size > self.in_buf.len() {
                // This may be the LG format trailer, EOF
                return Ok(0);
            }

            // Read the entire compressed block
            let compressed_block = &mut self.in_buf[..block_size];
            if let Ok(len) = self.read.read(compressed_block) {
                if len == 0 {
                    // We hit EOF, that's fine
                    return Ok(0);
                } else if len != block_size {
                    let remain = &mut compressed_block[len..];
                    self.read.read_exact(remain)?;
                }
            }

            self.out_len = lz4::block::decompress_to_buffer(
                compressed_block,
                Some(LZ4_BLOCK_SIZE as i32),
                &mut self.out_buf,
            )?;
            self.out_pos = 0;
        }
        let copy_len = min(buf.len(), self.out_len - self.out_pos);
        buf[..copy_len].copy_from_slice(&self.out_buf[self.out_pos..self.out_pos + copy_len]);
        self.out_pos += copy_len;
        Ok(copy_len)
    }
}

pub fn get_decoder<'a, R: Read + 'a>(
    format: CompressFormat,
    r: R,
) -> anyhow::Result<Box<dyn Read + 'a>> {
    Ok(match format {
        CompressFormat::XZ => Box::new(XzReader::new(r, true)),
        CompressFormat::LZMA => Box::new(LzmaReader::new_mem_limit(r, u32::MAX, None)?),
        CompressFormat::BZIP2 => Box::new(BzDecoder::new(r)),
        CompressFormat::LZ4 => Box::new(LZ4FrameDecoder::new(r)?),
        CompressFormat::LZ4_LEGACY => Box::new(LZ4BlockDecoder::new(r)),
        CompressFormat::ZOPFLI | CompressFormat::GZIP => Box::new(MultiGzDecoder::new(r)),
        _ => unreachable!(),
    })
}

pub fn get_encoder<'a, W: Write + ?Sized>(
    format: CompressFormat,
    w: &'a mut W,
) -> std::io::Result<Box<dyn WriteFinish<&'a mut W> + 'a>> {
    Ok(match format {
        CompressFormat::XZ => {
            let mut opt = XzOptions::with_preset(9);
            opt.set_check_sum_type(CheckType::Crc32);
            Box::new(XzWriter::new(w, opt)?)
        }
        CompressFormat::LZMA => Box::new(LzmaWriter::new_use_header(
            w,
            &LzmaOptions::with_preset(9),
            None,
        )?),
        CompressFormat::BZIP2 => Box::new(BzEncoder::new(w, BzCompression::best())),
        CompressFormat::LZ4 => {
            let encoder = LZ4FrameEncoderBuilder::new()
                .block_size(BlockSize::Max4MB)
                .block_mode(BlockMode::Independent)
                .checksum(ContentChecksum::ChecksumEnabled)
                .block_checksum(BlockChecksum::BlockChecksumEnabled)
                .level(9)
                .auto_flush(true)
                .build(w)?;
            Box::new(encoder)
        }
        CompressFormat::LZ4_LEGACY => Box::new(LZ4BlockEncoder::new(w, false)),
        // CompressFormat::LZ4_LG => Box::new(LZ4BlockEncoder::new(w, true)),
        CompressFormat::ZOPFLI => {
            // These options are already better than gzip -9
            let opt = ZopfliOptions {
                iteration_count: unsafe { NonZeroU64::new_unchecked(1) },
                maximum_block_splits: 1,
                ..Default::default()
            };
            Box::new(ZopFliEncoder::new_buffered(opt, BlockType::Dynamic, w)?)
        }
        CompressFormat::GZIP => Box::new(GzEncoder::new(w, GzCompression::best())),
        _ => unreachable!(),
    })
}
