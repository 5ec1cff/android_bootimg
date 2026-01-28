use bytemuck::{Pod, bytes_of, bytes_of_mut};
use std::cmp::min;
use std::fmt::{Debug, Display, LowerHex};
use std::io;
use std::io::{Read, Write};
use std::mem::MaybeUninit;

// https://github.com/topjohnwu/Magisk/blob/0bbc7360519726f7e3b5004542c0131fa0c0c86f/native/src/base/files.rs#L24-L128

pub trait ReadExt {
    #[allow(unused)]
    fn skip(&mut self, len: usize) -> io::Result<()>;
    fn read_pod<F: Pod>(&mut self, data: &mut F) -> io::Result<()>;
}

impl<T: Read> ReadExt for T {
    #[allow(unused)]
    fn skip(&mut self, mut len: usize) -> io::Result<()> {
        let mut buf = MaybeUninit::<[u8; 4096]>::uninit();
        let buf = unsafe { buf.assume_init_mut() };
        while len > 0 {
            let l = min(buf.len(), len);
            self.read_exact(&mut buf[..l])?;
            len -= l;
        }
        Ok(())
    }

    fn read_pod<F: Pod>(&mut self, data: &mut F) -> io::Result<()> {
        self.read_exact(bytes_of_mut(data))
    }
}

pub trait WriteExt {
    #[allow(unused)]
    fn write_zeros(&mut self, len: usize) -> io::Result<usize>;
    fn write_pod<F: Pod>(&mut self, data: &F) -> io::Result<()>;

    fn write_all_size(&mut self, data: &[u8]) -> io::Result<usize>;
}

impl<T: Write> WriteExt for T {
    fn write_zeros(&mut self, mut len: usize) -> io::Result<usize> {
        let buf = [0_u8; 4096];
        let orig_len = len;
        while len > 0 {
            let l = min(buf.len(), len);
            self.write_all(&buf[..l])?;
            len -= l;
        }
        Ok(orig_len)
    }

    fn write_pod<F: Pod>(&mut self, data: &F) -> io::Result<()> {
        self.write_all(bytes_of(data))
    }

    fn write_all_size(&mut self, data: &[u8]) -> io::Result<usize> {
        self.write_all(data)?;
        Ok(data.len())
    }
}

pub struct Chunker {
    chunk: Box<[u8]>,
    chunk_size: usize,
    pos: usize,
}

impl Chunker {
    pub fn new(chunk_size: usize) -> Self {
        Chunker {
            // SAFETY: all bytes will be initialized before it is used, tracked by self.pos
            chunk: unsafe { Box::new_uninit_slice(chunk_size).assume_init() },
            chunk_size,
            pos: 0,
        }
    }

    #[allow(unused)]
    pub fn set_chunk_size(&mut self, chunk_size: usize) {
        self.chunk_size = chunk_size;
        self.pos = 0;
        if self.chunk.len() < chunk_size {
            self.chunk = unsafe { Box::new_uninit_slice(chunk_size).assume_init() };
        }
    }

    // Returns (remaining buf, Option<Chunk>)
    pub fn add_data<'a, 'b: 'a>(&'a mut self, mut buf: &'b [u8]) -> (&'b [u8], Option<&'a [u8]>) {
        let mut chunk = None;
        if self.pos > 0 {
            // Try to fill the chunk
            let len = std::cmp::min(self.chunk_size - self.pos, buf.len());
            self.chunk[self.pos..self.pos + len].copy_from_slice(&buf[..len]);
            self.pos += len;
            // If the chunk is filled, consume it
            if self.pos == self.chunk_size {
                chunk = Some(&self.chunk[..self.chunk_size]);
                self.pos = 0;
            }
            buf = &buf[len..];
        } else if buf.len() >= self.chunk_size {
            // Directly consume a chunk from buf
            chunk = Some(&buf[..self.chunk_size]);
            buf = &buf[self.chunk_size..];
        } else {
            // Copy buf into chunk
            self.chunk[self.pos..self.pos + buf.len()].copy_from_slice(buf);
            self.pos += buf.len();
            return (&[], None);
        }
        (buf, chunk)
    }

    pub fn get_available(&mut self) -> &[u8] {
        let chunk = &self.chunk[..self.pos];
        self.pos = 0;
        chunk
    }
}

pub fn align_to<N: num_traits::PrimInt + Display + Debug + LowerHex>(num: N, alignment: N) -> N {
    let one = N::from(1).unwrap();
    assert_eq!(
        alignment & (alignment - one),
        N::from(0).unwrap(),
        "invalid alignment 0x{:x}",
        alignment
    );
    (num + alignment - one) & !(alignment - one)
}

pub trait SliceExt {
    fn u32_at(&self, offset: usize) -> Option<u32>;
}

impl SliceExt for [u8] {
    fn u32_at(&self, offset: usize) -> Option<u32> {
        self.get(offset..offset + 4)
            .map(|data| u32::from_le_bytes(data.try_into().unwrap()))
    }
}

pub fn trim_end(data: &[u8]) -> &[u8] {
    &data[..data.iter().position(|&b| b == 0).unwrap_or(data.len())]
}
