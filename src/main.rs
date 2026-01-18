mod compress;
mod layouts;
mod constants;

use crate::compress::{get_decoder, parse_compress_format, CompressFormat};
use crate::layouts::{BootHeaderLayout, BOOT_HEADER_V0, BOOT_HEADER_V1, BOOT_HEADER_V2, BOOT_HEADER_V3, BOOT_HEADER_V4, VENDOR_BOOT_HEADER_V3, VENDOR_BOOT_HEADER_V4};
use crate::BootImageVersion::{Android, Vendor};
use anyhow::{bail, Result};
use memmap2::Mmap;
use paste::paste;
use std::cmp::PartialEq;
use std::env;
use std::fmt::{Display, Formatter};
use std::fs::File;
use std::io::{Read, Write};
use std::ops::{Deref, DerefMut};

const BOOT_MAGIC: &[u8] = b"ANDROID!";
const VENDOR_BOOT_MAGIC: &[u8] = b"VNDRBOOT";


#[derive(Debug)]
enum BootImageVersion {
    Android(u32),
    Vendor(u32),
}

#[derive(Debug)]
struct BootImage<'a> {
    data: &'a [u8],
    version: BootImageVersion,
    layout: &'static BootHeaderLayout,
}


trait SliceExt {
    fn u32_at(&self, offset: usize) -> Result<u32>;
}

impl SliceExt for [u8] {
    fn u32_at(&self, offset: usize) -> Result<u32> {
        if let Some(data) = self.get(offset..offset + 4) {
            return Ok(u32::from_le_bytes(data.try_into()?));
        }

        bail!("Invalid offset 0x{:08x}", offset)
    }
}

struct OsVersion {
    a: u32,
    b: u32,
    c: u32
}

impl Display for OsVersion {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("{}.{}.{}", self.a, self.b, self.c))
    }
}

struct PatchLevel {
    year: u32,
    month: u32,
}

impl Display for PatchLevel {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("{}-{}", self.year, self.month))
    }
}

fn parse_os_version(version: u32) -> (OsVersion, PatchLevel) {
    let os_ver = version >> 11;
    let patch_level = version & 0x7ff;

    let a = (os_ver >> 14) & 0x7f;
    let b = (os_ver >> 7) & 0x7f;
    let c = os_ver & 0x7f;

    let y = (patch_level >> 4) + 2000;
    let m = patch_level & 0xf;

    (OsVersion { a, b, c }, PatchLevel { year: y, month: m })
}

struct BootImageBlocks<'a> {
    kernel: Option<&'a [u8]>,
    ramdisk: Option<&'a [u8]>,
    second: Option<&'a [u8]>,
    // TODO: extra
    recovery_dtbo: Option<&'a [u8]>,
    dtb: Option<&'a [u8]>,
    signature: Option<&'a [u8]>,
    vendor_ramdisk_table: Option<&'a [u8]>,
    bootconfig: Option<&'a [u8]>,
}

impl BootImageBlocks<'_> {
    fn dump_kernel(&self, out: &mut dyn Write, raw: bool) -> Result<()> {
        if let Some(mut kernel) = self.kernel {
            if raw {
                std::io::copy(&mut kernel, out)?;
            } else {
                let format = parse_compress_format(kernel);
                if format != CompressFormat::UNKNOWN {
                    let mut decoder = get_decoder(format, kernel)?;
                    std::io::copy(decoder.as_mut(), out)?;
                } else {
                    std::io::copy(&mut kernel, out)?;
                }
            }
        }

        Ok(())
    }
}

fn align_to(num: usize, alignment: usize) -> usize {
    assert_eq!(alignment & (alignment - 1), 0, "invalid alignment 0x{:x}", alignment);
    (num + alignment - 1) & !(alignment - 1)
}

impl<'a> BootImage<'a> {
    fn parse(data: &'a [u8]) -> Result<Self> {
        if data.starts_with(BOOT_MAGIC) {
            if let Ok(version) = BOOT_HEADER_V0.get_header_version(data) {
                return Ok(BootImage {
                    data,
                    version: Android(version),
                    layout: match version {
                        0 => &BOOT_HEADER_V0,
                        1 => &BOOT_HEADER_V1,
                        2 => &BOOT_HEADER_V2,
                        3 => &BOOT_HEADER_V3,
                        4 => &BOOT_HEADER_V4,
                        _ => bail!("unsupported boot version {}", version)
                    }
                });
            }
        } else if data.starts_with(VENDOR_BOOT_MAGIC) {
            if let Ok(version) = VENDOR_BOOT_HEADER_V3.get_header_version(data) {
                return Ok(BootImage {
                    data,
                    version: Vendor(version),
                    layout: match version {
                        3 => &VENDOR_BOOT_HEADER_V3,
                        4 => &VENDOR_BOOT_HEADER_V4,
                        _ => bail!("unsupported vendor boot version {}", version)
                    }
                })
            }
        }
        bail!("invalid boot image");
    }

    fn page_size(&self) -> usize {
        match self.version {
            Android(v) => {
                if v >= 3 {
                    return 4096;
                }
            },
            _ => {}
        }

        self.layout.get_page_size(self.data).unwrap() as usize
    }

    fn hdr_space(&self) -> usize {
        align_to(self.layout.total_size as usize, self.page_size())
    }

    fn get_blocks(&self) -> Result<BootImageBlocks<'a>> {
        let mut off = self.hdr_space();
        let page_size = self.page_size();

        macro_rules! build_blocks {
            ($($name:ident),*) => {
                paste! {
                    $(
                        #[allow(unused)]
                        let $name = if self.layout.[<has_ $name _size>]() {
                            let block_size = self.layout.[<get_ $name _size>](self.data)?;
                            let size = block_size as usize;
                            if size > 0 {
                                if let Some(slice) = self.data.get(off..off + size) {
                                    println!("block {} at off {} sz {}", stringify!($name), off, block_size);
                                    off += align_to(size, page_size);
                                    Some(slice)
                                } else {
                                    bail!("invalid block {} off {} size {}", stringify!($name), off, size)
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                    )*

                    Ok(BootImageBlocks { $($name),* })
                }
            }
        }

        build_blocks! {
            kernel,
            ramdisk,
            second,
            // extra
            recovery_dtbo,
            dtb,
            signature,
            vendor_ramdisk_table,
            bootconfig
        }
    }

    fn print_info(&self) -> Result<()> {
        macro_rules! print_info_item {
            ($self: ident, $name:ident) => {
                paste! {
                    if $self.layout.[<has_ $name>]() {
                        let d = $self.layout.[<get_ $name>]($self.data)?;
                        println!("{}: {}", stringify!($name), d);
                    }
                }
            }
        }

        print_info_item!{ self, kernel_size }
        print_info_item!{ self, ramdisk_size }
        print_info_item!{ self, second_size }
        print_info_item!{ self, page_size }
        print_info_item!{ self, header_version }
        if self.layout.has_os_version() {
            if let Ok(d) = self.layout.get_os_version(self.data) {
                if d != 0 {
                    let (os_version, patch_level) = parse_os_version(d);
                    println!("os_version: {}", os_version);
                    println!("patch_level: {}", patch_level);
                }
            }
        }
        print_info_item!{ self, recovery_dtbo_size }
        print_info_item!{ self, recovery_dtbo_offset }
        print_info_item!{ self, header_size }
        print_info_item!{ self, dtb_size }

        print_info_item!{ self, signature_size }

        print_info_item!{ self, vendor_ramdisk_table_size }
        print_info_item!{ self, vendor_ramdisk_table_entry_num }
        print_info_item!{ self, vendor_ramdisk_table_entry_size }
        print_info_item!{ self, bootconfig_size }

        Ok(())
    }
}

fn main() -> Result<()> {
    if let Some(s) = env::args().skip(1).next() {
        let file = File::open(s)?;
        let mem = unsafe { Mmap::map(&file)? };
        let boot = BootImage::parse(&mem)?;

        println!("version: {:?}", boot.version);
        println!("layout: {:?}", boot.layout);
        boot.print_info()?;
        let blocks = boot.get_blocks()?;
        if let Some(kernel) = blocks.kernel {
            let fmt = parse_compress_format(kernel);
            println!("kernel format: {:?}", fmt);
        }

        if let Some(ramdisk) = blocks.ramdisk {
            let fmt = parse_compress_format(ramdisk);
            println!("ramdisk format: {:?}", fmt);
        }

        Ok(())
    } else {
        bail!("no file provided")
    }
}
