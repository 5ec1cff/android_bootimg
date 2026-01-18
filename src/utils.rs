use bytemuck::{bytes_of_mut, Pod};
use std::cmp::min;
use std::io;
use std::io::Read;
use std::mem::MaybeUninit;

pub trait ReadExt {
    fn skip(&mut self, len: usize) -> io::Result<()>;
    fn read_pod<F: Pod>(&mut self, data: &mut F) -> io::Result<()>;
}

impl<T: Read> ReadExt for T {
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

pub fn align_to(num: usize, alignment: usize) -> usize {
    assert_eq!(alignment & (alignment - 1), 0, "invalid alignment 0x{:x}", alignment);
    (num + alignment - 1) & !(alignment - 1)
}
