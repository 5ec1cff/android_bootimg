use crate::utils::{WriteExt, align_to};
use anyhow::{Result, anyhow, bail};
use itertools::Itertools;
use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::io::{Cursor, Read, Write};
use std::ops::Deref;
use std::{io, str};

pub struct Cpio {
    entries: BTreeMap<String, Box<CpioEntry>>,
}

pub struct CpioEntry {
    mode: u32,
    uid: u32,
    gid: u32,
    rdev_major: u32,
    rdev_minor: u32,
    data: Option<Box<dyn AsRef<[u8]>>>,
}

pub const TYPE_MASK: u32 = 0o170000;
pub const TYPE_FIFO: u32 = 0o010000;
pub const TYPE_CHAR: u32 = 0o020000;
pub const TYPE_DIR: u32 = 0o040000;
pub const TYPE_BLOCK: u32 = 0o060000;
pub const TYPE_REGULAR: u32 = 0o100000;
pub const TYPE_NETWORK_SPECIAL: u32 = 0o110000;
pub const TYPE_SYMLINK: u32 = 0o120000;
pub const TYPE_SOCKET: u32 = 0o140000;

fn read_hex_u32<R: Read>(reader: &mut R) -> io::Result<u32> {
    let mut bytes = [0u8; 8];
    reader.read_exact(&mut bytes)?;
    str::from_utf8(&bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid utf-8 header field"))
        .and_then(|string| {
            u32::from_str_radix(string, 16).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "Invalid hex u32 header field")
            })
        })
}

impl Cpio {
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    pub fn load_from_data(data: &[u8]) -> Result<Self> {
        let mut cpio = Cpio::new();
        let mut cursor = Cursor::new(data);
        loop {
            let mut magic = [0u8; 6];
            cursor.read_exact(&mut magic)?;
            if magic.as_slice() != b"070701" {
                bail!("unsupported cpio header")
            }

            let _ino = read_hex_u32(&mut cursor)?;
            let mode = read_hex_u32(&mut cursor)?;
            let uid = read_hex_u32(&mut cursor)?;
            let gid = read_hex_u32(&mut cursor)?;
            let _nlink = read_hex_u32(&mut cursor)?;
            let _mtime = read_hex_u32(&mut cursor)?;
            let file_size = read_hex_u32(&mut cursor)?;
            let _dev_major = read_hex_u32(&mut cursor)?;
            let _dev_minor = read_hex_u32(&mut cursor)?;
            let rdev_major = read_hex_u32(&mut cursor)?;
            let rdev_minor = read_hex_u32(&mut cursor)?;
            let name_len = read_hex_u32(&mut cursor)? as usize;
            let _checksum = read_hex_u32(&mut cursor)?;

            // NUL-terminated name with length `name_len` (including NUL byte).
            let mut name_bytes = vec![0u8; name_len];
            cursor.read_exact(&mut name_bytes)?;
            if name_bytes.last() != Some(&0) {
                bail!("Entry name was not NUL-terminated")
            }
            name_bytes.pop();
            while name_bytes.last() == Some(&0) {
                name_bytes.pop();
            }
            let name = String::from_utf8(name_bytes)?;
            cursor.set_position(align_to(cursor.position(), 4));
            if name == "." || name == ".." {
                continue;
            }
            if name == "TRAILER!!!" {
                match data[cursor.position() as usize..]
                    .windows(6)
                    .position(|h| h == b"070701")
                {
                    Some(x) => cursor.set_position(cursor.position() + x as u64),
                    None => break,
                }
                continue;
            }
            let mut file_data = vec![0u8; file_size as usize];
            cursor.read_exact(&mut file_data)?;
            let entry = Box::new(CpioEntry {
                mode,
                uid,
                gid,
                rdev_major,
                rdev_minor,
                data: Some(Box::new(file_data)),
            });
            cpio.entries.insert(name, entry);
            cursor.set_position(align_to(cursor.position(), 4));
        }
        Ok(cpio)
    }

    pub fn dump(&self, output: &mut dyn Write) -> Result<()> {
        let mut output = output;
        let mut pos = 0usize;
        let mut inode = 300000i64;
        for (name, entry) in &self.entries {
            pos += output.write(
                format!(
                    "070701{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}",
                    inode,
                    entry.mode,
                    entry.uid,
                    entry.gid,
                    1,
                    0,
                    entry.len(),
                    0,
                    0,
                    entry.rdev_major,
                    entry.rdev_minor,
                    name.len() + 1,
                    0
                ).as_bytes(),
            )?;
            pos += output.write(name.as_bytes())?;
            pos += output.write(&[0])?;
            output.write_zeros(align_to(pos, 4) - pos)?;
            pos = align_to(pos, 4);
            if let Some(data) = entry.data.as_ref() {
                pos += output.write(data.as_ref().as_ref())?;
                output.write_zeros(align_to(pos, 4))?;
                pos = align_to(pos, 4);
            }
            inode += 1;
        }
        pos += output.write(
            format!("070701{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}",
                    inode, 0o755, 0, 0, 1, 0, 0, 0, 0, 0, 0, 11, 0
            ).as_bytes()
        )?;
        pos += output.write("TRAILER!!!\0".as_bytes())?;
        output.write_zeros(align_to(pos, 4))?;
        Ok(())
    }

    pub fn rm(&mut self, path: &str, recursive: bool) {
        let path = norm_path(path);
        self.entries.remove(&path);
        if recursive {
            let path = path + "/";
            self.entries
                .retain(|k, _| if k.starts_with(&path) { false } else { true })
        }
    }

    pub fn exists(&self, path: &str) -> bool {
        self.entries.contains_key(&norm_path(path))
    }

    pub fn add(&mut self, path: &str, entry: CpioEntry) -> Result<()> {
        if path.ends_with('/') {
            bail!("path cannot end with / for add")
        }

        self.entries.insert(norm_path(path), Box::new(entry));
        Ok(())
    }

    pub fn mv(&mut self, from: &str, to: &str) -> Result<()> {
        let entry = self
            .entries
            .remove(&norm_path(from))
            .ok_or_else(|| anyhow!("No such entry {from}"))?;
        self.entries.insert(norm_path(to), entry);
        Ok(())
    }

    pub fn ls(&self, path: &str, recursive: bool) {
        let path = norm_path(path);
        let path = if path.is_empty() {
            path
        } else {
            "/".to_string() + path.as_str()
        };
        for (name, entry) in &self.entries {
            let p = "/".to_string() + name.as_str();
            let Some(p) = p.strip_prefix(&path) else {
                continue;
            };
            if !p.is_empty() && !p.starts_with('/') {
                continue;
            }
            if !recursive && !p.is_empty() && p.matches('/').count() > 1 {
                continue;
            }
            println!("{entry}\t{name}");
        }
    }

    pub fn entries(&self) -> &BTreeMap<String, Box<CpioEntry>> {
        &self.entries
    }

    pub fn entry_by_name(&self, name: &str) -> Option<&CpioEntry> {
        self.entries.get(name).map(|x| x.deref())
    }
}

impl Cpio {
    pub fn is_magisk_patched(&self) -> bool {
        for file in [
            ".backup/.magisk",
            "init.magisk.rc",
            "overlay/init.magisk.rc",
        ] {
            if self.exists(file) {
                return true;
            }
        }
        false
    }
}

impl Display for CpioEntry {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}{}{}{}{}{}{}{}{}{}\t{}\t{}\t{}\t{}:{}",
            match self.mode & TYPE_MASK {
                TYPE_DIR => "d",
                TYPE_REGULAR => "-",
                TYPE_SYMLINK => "l",
                TYPE_BLOCK => "b",
                TYPE_CHAR => "c",
                _ => "?",
            },
            if self.mode & 0o400 != 0 { "r" } else { "-" },
            if self.mode & 0o200 != 0 { "w" } else { "-" },
            if self.mode & 0o100 != 0 { "x" } else { "-" },
            if self.mode & 0o040 != 0 { "r" } else { "-" },
            if self.mode & 0o020 != 0 { "w" } else { "-" },
            if self.mode & 0o010 != 0 { "x" } else { "-" },
            if self.mode & 0o004 != 0 { "r" } else { "-" },
            if self.mode & 0o002 != 0 { "w" } else { "-" },
            if self.mode & 0o001 != 0 { "x" } else { "-" },
            self.uid,
            self.gid,
            self.len(),
            self.rdev_major,
            self.rdev_minor,
        )
    }
}

#[inline(always)]
fn norm_path(path: &str) -> String {
    Itertools::intersperse(path.split('/').filter(|x| !x.is_empty()), "/").collect()
}

impl CpioEntry {
    pub fn len(&self) -> usize {
        self.data
            .as_ref()
            .map(|d| d.as_ref().as_ref().len())
            .unwrap_or(0)
    }

    pub fn data(&self) -> Option<&[u8]> {
        self.data.as_ref().map(|x| x.deref().as_ref())
    }

    pub fn regular(mode: u32, data: Box<dyn AsRef<[u8]>>) -> Self {
        Self {
            mode: mode | TYPE_REGULAR,
            uid: 0,
            gid: 0,
            rdev_major: 0,
            rdev_minor: 0,
            data: Some(data),
        }
    }

    pub fn dir(mode: u32) -> Self {
        Self {
            mode: mode | TYPE_DIR,
            uid: 0,
            gid: 0,
            rdev_major: 0,
            rdev_minor: 0,
            data: None,
        }
    }

    pub fn symlink(mode: u32, src: &str) -> Self {
        Self {
            mode: mode | TYPE_SYMLINK,
            uid: 0,
            gid: 0,
            rdev_major: 0,
            rdev_minor: 0,
            data: Some(Box::new(norm_path(src).as_bytes().to_vec())),
        }
    }

    pub fn char(mode: u32, rdev_major: u32, rdev_minor: u32) -> Self {
        Self {
            mode: mode | TYPE_CHAR,
            uid: 0,
            gid: 0,
            rdev_major,
            rdev_minor,
            data: None,
        }
    }

    pub fn uid(self, uid: u32) -> Self {
        Self { uid, ..self }
    }

    pub fn gid(self, gid: u32) -> Self {
        Self { gid, ..self }
    }
}
