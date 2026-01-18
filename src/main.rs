mod compress;
mod layouts;
mod constants;
mod utils;

use crate::compress::{get_decoder, parse_compress_format, CompressFormat};
use crate::layouts::{BootHeaderLayout, BOOT_HEADER_V0, BOOT_HEADER_V1, BOOT_HEADER_V2, BOOT_HEADER_V3, BOOT_HEADER_V4, VENDOR_BOOT_HEADER_V3, VENDOR_BOOT_HEADER_V4};
use crate::utils::{align_to, ReadExt};
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


#[derive(Debug)]
enum BootImageVersion {
    Android(u32),
    Vendor(u32),
}

struct BootHeader<'a> {
    data: &'a [u8],
    layout: &'static BootHeaderLayout,
    version: BootImageVersion,
}

macro_rules! impl_ifield_accessor {
    ($vis:vis, $t:ty, $name:ident $(,$suffix:ident)?) => {
        paste! {
            #[allow(unused)]
            $vis fn [<has_ $name $($suffix)?>](&self) -> bool {
                return self.layout.[<offset_ $name>] != 0;
            }
            #[allow(unused)]
            $vis fn [<get_ $name $($suffix)?>](&self) -> $t {
                let offset = self.layout.[<offset_ $name>] as usize;
                return $t::from_le_bytes(self.data[offset..offset + 4].try_into().unwrap());
            }
        }
    };
}

macro_rules! impl_sfield_accessor {
    ($vis:vis, $name:ident $(,$suffix:ident)?) => {
        paste! {
            #[allow(unused)]
            $vis fn [<has_ $name $($suffix)?>](&self) -> bool {
                return self.layout.[<offset_ $name>] != 0;
            }
            #[allow(unused)]
            $vis fn [<get_ $name $($suffix)?>](&self) -> &[u8] {
                let offset = self.layout.[<offset_ $name>] as usize;
                let sz = self.layout.[<size_ $name>] as usize;
                return &self.data[offset..offset + sz];
            }
        }
    };
}

impl<'a> BootHeader<'a> {
    impl_ifield_accessor! { pub, u32, kernel_size }
    impl_ifield_accessor! { pub, u32, ramdisk_size }
    impl_ifield_accessor! { pub, u32, second_size }
    impl_ifield_accessor! { pub, u32, page_size }
    impl_ifield_accessor! { pub, u32, header_version }
    impl_ifield_accessor! { pub, u32, os_version, _raw }
    impl_ifield_accessor! { pub, u32, recovery_dtbo_size }
    impl_ifield_accessor! { pub, u64, recovery_dtbo_offset }
    impl_ifield_accessor! { pub, u32, header_size }
    impl_ifield_accessor! { pub, u32, dtb_size }
    impl_ifield_accessor! { pub, u32, signature_size }
    impl_ifield_accessor! { pub, u32, vendor_ramdisk_table_size }
    impl_ifield_accessor! { pub, u32, vendor_ramdisk_table_entry_num }
    impl_ifield_accessor! { pub, u32, vendor_ramdisk_table_entry_size }
    impl_ifield_accessor! { pub, u32, bootconfig_size }
    impl_sfield_accessor! { pub, name }
    impl_sfield_accessor! { pub, cmdline }
    impl_sfield_accessor! { pub, id }
    impl_sfield_accessor! { pub, extra_cmdline }

    pub fn get_os_version(&self) -> Option<(OsVersion, PatchLevel)> {
        let version = self.get_os_version_raw();
        if version == 0 {
            return None;
        }
        let os_ver = version >> 11;
        let patch_level = version & 0x7ff;

        let a = (os_ver >> 14) & 0x7f;
        let b = (os_ver >> 7) & 0x7f;
        let c = os_ver & 0x7f;

        let y = (patch_level >> 4) + 2000;
        let m = patch_level & 0xf;

        Some((OsVersion { a, b, c }, PatchLevel { year: y, month: m }))
    }

    pub fn page_size(&self) -> usize {
        match self.version {
            Android(v) => {
                if v >= 3 {
                    return 4096;
                }
            },
            _ => {}
        }

        self.get_page_size() as usize
    }

    pub fn hdr_space(&self) -> usize {
        // TODO: only vendor boot has page count > 1
        align_to(self.layout.total_size as usize, self.page_size())
    }

    pub fn parse(data: &'a [u8]) -> Result<Self> {
        if data.starts_with(BOOT_MAGIC) {
            let mut version = u32::MAX;
            let mut tmp = data;
            tmp.skip(BOOT_HEADER_V0.offset_header_version as usize)?;
            tmp.read_pod(&mut version)?;

            let layout = match version {
                0 => &BOOT_HEADER_V0,
                1 => &BOOT_HEADER_V1,
                2 => &BOOT_HEADER_V2,
                3 => &BOOT_HEADER_V3,
                4 => &BOOT_HEADER_V4,
                _ => bail!("unsupported boot version {}", version)
            };

            let data = &data[..layout.total_size as usize];

            Ok(Self {
                data,
                layout,
                version: Android(version),
            })
        } else if data.starts_with(VENDOR_BOOT_MAGIC) {
            let mut version = u32::MAX;
            let mut tmp = data;
            tmp.skip(VENDOR_BOOT_HEADER_V3.offset_header_version as usize)?;
            tmp.read_pod(&mut version)?;
            let layout = match version {
                3 => &VENDOR_BOOT_HEADER_V3,
                4 => &VENDOR_BOOT_HEADER_V4,
                _ => bail!("unsupported vendor boot version {}", version)
            };

            let data = &data[..layout.total_size as usize];

            Ok(Self {
                data,
                layout,
                version: Vendor(version),
            })
        } else {
            bail!("invalid boot image")
        }
    }
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

impl<'a> BootImageBlocks<'a> {
    pub fn new(data: &'a [u8], boot_header: &BootHeader) -> Result<Self> {
        let mut off = boot_header.hdr_space();
        let page_size = boot_header.page_size();

        macro_rules! build_blocks {
            ($($name:ident),*) => {
                paste! {
                    $(
                        #[allow(unused)]
                        let $name = if boot_header.[<has_ $name _size>]() {
                            let block_size = boot_header.[<get_ $name _size>]();
                            let size = block_size as usize;
                            if size > 0 {
                                if let Some(slice) = data.get(off..off + size) {
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
            // TODO: extra
            recovery_dtbo,
            dtb,
            signature,
            vendor_ramdisk_table,
            bootconfig
        }
    }

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

struct BootImage<'a> {
    data: &'a [u8],
    header: BootHeader<'a>,
    blocks: BootImageBlocks<'a>,
}

impl<'a> BootImage<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let header = BootHeader::parse(data)?;
        let blocks = BootImageBlocks::new(data, &header)?;

        Ok(Self { data, header, blocks })
    }

    fn print_info(&self) -> Result<()> {
        macro_rules! print_info_item {
            ($self: ident, $name:ident) => {
                paste! {
                    if $self.header.[<has_ $name>]() {
                        let d = $self.header.[<get_ $name>]();
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
        if self.header.has_os_version_raw() {
            if let Some((os_version, patch_level)) = self.header.get_os_version() {
                println!("os_version: {}", os_version);
                println!("patch_level: {}", patch_level);
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

        println!("version: {:?}", boot.header.version);
        println!("layout: {:?}", boot.header.layout);
        boot.print_info()?;
        if let Some(kernel) = boot.blocks.kernel {
            let fmt = parse_compress_format(kernel);
            println!("kernel format: {:?}", fmt);
        }

        if let Some(ramdisk) = boot.blocks.ramdisk {
            let fmt = parse_compress_format(ramdisk);
            println!("ramdisk format: {:?}", fmt);
        }

        Ok(())
    } else {
        bail!("no file provided")
    }
}
