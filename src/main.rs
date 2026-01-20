mod compress;
mod layouts;
mod constants;
mod utils;

use crate::compress::{get_decoder, get_encoder, parse_compress_format, CompressFormat};
use crate::constants::{AVB_FOOTER_MAGIC, AVB_MAGIC};
use crate::layouts::{AvbFooter, BootHeaderLayout, VendorRamdiskTableEntryType, VendorRamdiskTableEntryV4, BOOT_HEADER_V0, BOOT_HEADER_V1, BOOT_HEADER_V2, BOOT_HEADER_V3, BOOT_HEADER_V4, VENDOR_BOOT_HEADER_V3, VENDOR_BOOT_HEADER_V4};
use crate::utils::{align_to, SliceExt};
use crate::BootImageVersion::{Android, Vendor};
use anyhow::{bail, Result};
use memmap2::Mmap;
use paste::paste;
use std::collections::HashMap;
use std::env;
use std::fmt::{Display, Formatter};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::ops::{Deref, DerefMut};
use std::str::from_utf8;

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
                return $t::from_le_bytes(self.data[offset..offset + size_of::<$t>()].try_into().unwrap());
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
        println!("ver: {:?}", self.version);
        align_to(self.layout.total_size as usize, self.page_size())
    }

    pub fn parse(data: &'a [u8]) -> Result<Self> {
        if data.starts_with(BOOT_MAGIC) {
            if let Some(version) =  data.u32_at(BOOT_HEADER_V0.offset_header_version as usize) {
                let layout = match version {
                    0 => &BOOT_HEADER_V0,
                    1 => &BOOT_HEADER_V1,
                    2 => &BOOT_HEADER_V2,
                    3 => &BOOT_HEADER_V3,
                    4 => &BOOT_HEADER_V4,
                    _ => bail!("unsupported boot version {}", version)
                };

                let data = &data[..layout.total_size as usize];

                return Ok(Self {
                    data,
                    layout,
                    version: Android(version),
                });
            }
        } else if data.starts_with(VENDOR_BOOT_MAGIC) {
            if let Some(version) =  data.u32_at(VENDOR_BOOT_HEADER_V3.offset_header_version as usize) {
                let layout = match version {
                    3 => &VENDOR_BOOT_HEADER_V3,
                    4 => &VENDOR_BOOT_HEADER_V4,
                    _ => bail!("unsupported vendor boot version {}", version)
                };

                let data = &data[..layout.total_size as usize];

                return Ok(Self {
                    data,
                    layout,
                    version: Vendor(version),
                });
            }
        }
        bail!("invalid boot image")
    }
}

struct KernelImage<'a> {
    data: &'a [u8],
    compress_format: CompressFormat,
}

struct RamdiskImage<'a> {
    data: &'a [u8],
    compress_format: CompressFormat,
    vendor_ramdisk_table: Option<Vec<VendorRamdiskEntry<'a>>>,
}

struct BootImageBlocks<'a> {
    kernel: Option<KernelImage<'a>>,
    ramdisk: Option<RamdiskImage<'a>>,
    second: Option<&'a [u8]>,
    // TODO: extra
    recovery_dtbo: Option<&'a [u8]>,
    dtb: Option<&'a [u8]>,
    signature: Option<&'a [u8]>,
    bootconfig: Option<&'a [u8]>,
}

impl<'a> BootImageBlocks<'a> {
    pub fn parse(data: &'a [u8], boot_header: &BootHeader) -> Result<(Self, usize)> {
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

        let kernel = if let Some(data) = kernel {
            Some(KernelImage {
                data,
                compress_format: parse_compress_format(data)
            })
        } else {
            None
        };

        let vendor_ramdisk_table = if let Some(entry_table) = &vendor_ramdisk_table {
            let entry_size = boot_header.get_vendor_ramdisk_table_entry_size() as usize;
            if entry_size != VendorRamdiskTableEntryV4::SIZE {
                bail!("invalid vendor ramdisk table entry size: {}", entry_size);
            }

            let entry_table_size = boot_header.get_vendor_ramdisk_table_entry_num() as usize * entry_size;

            if entry_table.len() < entry_table_size {
                bail!("invalid vendor ramdisk table entry size: {}", entry_table.len());
            }

            let entry_table = &entry_table[..entry_table_size];

            if ramdisk.is_none() {
                bail!("missing ramdisk")
            }

            let ramdisk = ramdisk.as_ref().unwrap();

            let mut vec = Vec::new();
            for d in entry_table.chunks(entry_size) {
                let entry_v4 = VendorRamdiskTableEntryV4 { data: d };

                let off = entry_v4.get_ramdisk_offset() as usize;
                let sz = entry_v4.get_ramdisk_size() as usize;
                if let Some(data) = ramdisk.get(off..off + sz) {
                    vec.push(VendorRamdiskEntry {
                        data,
                        entry_size: sz as u64,
                        entry_offset: off as u64,
                        entry_type: entry_v4.get_ramdisk_type(),
                        compress_format: parse_compress_format(data),
                        entry: entry_v4,
                    })
                } else {
                    bail!("invalid vendor ramdisk entry off={} size={}", off, sz);
                }
            }

            Some(vec)
        } else {
            None
        };

        let ramdisk = if let Some(data) = ramdisk {
            Some(RamdiskImage {
                data,
                compress_format: if vendor_ramdisk_table.is_none() {
                    parse_compress_format(data)
                } else {
                    CompressFormat::UNKNOWN
                },
                vendor_ramdisk_table
            })
        } else {
            None
        };


        Ok((
            BootImageBlocks {
                kernel, ramdisk, second, recovery_dtbo, dtb, signature, bootconfig
            },
            off
        ))
    }
}

struct BootImageAVBInfo<'a> {
    avb_tail: Option<&'a [u8]>,
    avb_header: &'a [u8],
    avb_footer: AvbFooter<'a>,
}

#[derive(Copy, Clone)]
struct VendorRamdiskEntry<'a> {
    data: &'a [u8],
    entry_offset: u64,
    entry_size: u64,
    entry_type: VendorRamdiskTableEntryType,
    compress_format: CompressFormat,
    entry: VendorRamdiskTableEntryV4<'a>,
}

struct BootImage<'a> {
    data: &'a [u8],
    header: BootHeader<'a>,
    blocks: BootImageBlocks<'a>,
    avb_info: Option<BootImageAVBInfo<'a>>,
}

fn dump_block(data: &[u8], out: &mut dyn Write, raw: bool) -> Result<()> {
    let mut data = data;
    if !raw {
        let format = parse_compress_format(data);
        if format != CompressFormat::UNKNOWN {
            let mut decoder = get_decoder(format, data)?;
            std::io::copy(decoder.as_mut(), out)?;
            return Ok(());
        }
    }
    std::io::copy(&mut data, out)?;

    Ok(())
}

impl<'a> BootImage<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let header = BootHeader::parse(data)?;
        let (blocks, tail) = BootImageBlocks::parse(data, &header)?;

        let avb_info = if let Some(avb_footer) = data.get(data.len() - AvbFooter::SIZE..) {
            if avb_footer.starts_with(AVB_FOOTER_MAGIC) {
                let avb_footer = AvbFooter { data: avb_footer };
                let off = avb_footer.get_vbmeta_offset() as usize;
                if let Some(avb_header) = data.get(off..off + avb_footer.get_vbmeta_size() as usize) {
                    if avb_header.starts_with(AVB_MAGIC) {
                        let avb_payload_size = avb_footer.get_original_image_size() as usize;
                        let avb_tail = if avb_payload_size > tail {
                            data.get(tail..avb_payload_size)
                        } else if avb_payload_size < tail {
                            bail!("invalid avb original image size")
                        } else {
                            None
                        };
                        Some(BootImageAVBInfo {
                            avb_tail,
                            avb_header,
                            avb_footer,
                        })
                    } else {
                        bail!("invalid avb header magic")
                    }
                } else {
                    bail!("invalid avb header")
                }
            } else {
                None
            }
        } else {
            None
        };

        Ok(Self { data, header, blocks, avb_info })
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

struct ReplacePayload {
    data: Box<dyn Read>,
    compressed: bool,
}

struct BootImagePatchOption<'a> {
    source_boot_image: &'a BootImage<'a>,
    replace_ramdisk: Option<ReplacePayload>,
    replace_kernel: Option<ReplacePayload>,
    replace_vendor_ramdisk: HashMap<usize, ReplacePayload>,
    // TODO: allow replace other blocks
    override_cmdline: Option<&'a [u8]>,
    override_os_version: Option<(OsVersion, PatchLevel)>,
}

trait BootImageOutput : Read + Write + Seek {
    fn truncate(&mut self, size: u64) -> std::io::Result<()>;
}

impl<'a> BootImagePatchOption<'a> {
    pub fn new(source_boot_image: &'a BootImage<'a>) -> Self {
        Self {
            source_boot_image,
            replace_ramdisk: None,
            replace_kernel: None,
            replace_vendor_ramdisk: HashMap::new(),
            override_cmdline: None,
            override_os_version: None,
        }
    }

    pub fn replace_ramdisk(&mut self, ramdisk: Box<dyn Read>, compressed: bool) -> &mut Self {
        self.replace_ramdisk = Some(ReplacePayload { data: ramdisk, compressed });
        self
    }

    pub fn replace_kernel(&mut self, kernel: Box<dyn Read>, compressed: bool) -> &mut Self {
        self.replace_kernel = Some(ReplacePayload { data: kernel, compressed });
        self
    }

    pub fn replace_vendor_ramdisk(&mut self, index: usize, ramdisk: Box<dyn Read>, compressed: bool) -> &mut Self {
        self.replace_vendor_ramdisk.insert(index, ReplacePayload { data: ramdisk, compressed });
        self
    }

    pub fn override_cmdline(&mut self, override_cmdline: &'a [u8]) -> &mut Self {
        self.override_cmdline = Some(override_cmdline);
        self
    }

    pub fn override_os_version(&mut self, override_os_version: (OsVersion, PatchLevel)) -> &mut Self {
        self.override_os_version = Some(override_os_version);
        self
    }

    pub fn patch(mut self, output: &mut dyn BootImageOutput) -> Result<()> {
        // TODO: chromeos
        output.truncate(self.source_boot_image.data.len() as u64)?;

        output.seek(SeekFrom::Start(0))?;

        let mut pos: u64 = 0;
        macro_rules! file_align_with {
            ($e:expr) => {
                pos = output.seek(SeekFrom::Start(align_to(pos, $e)))?;
            };
        }

        macro_rules! file_align {
            () => {
                file_align_with!(self.source_boot_image.header.page_size() as u64);
            };
        }

        let header_off = output.seek(SeekFrom::Current(0))?;
        output.write_all(&self.source_boot_image.data[..self.source_boot_image.header.hdr_space()])?;
        pos += self.source_boot_image.header.hdr_space() as u64;
        println!("header space {}", pos);

        let kernel_off = pos;
        let kernel_source: Option<(Box<dyn Read>, bool)> = if let Some(payload) = self.replace_kernel {
            Some((payload.data, payload.compressed))
        } else if let Some(kernel) = &self.source_boot_image.blocks.kernel {
            Some((Box::new(kernel.data), true))
        } else {
            None
        };

        let kernel_size = if let Some((mut kernel_source, compressed)) = kernel_source {
            let format = if compressed {
                CompressFormat::UNKNOWN
            } else {
                if let Some(orig) = &self.source_boot_image.blocks.kernel {
                    orig.compress_format
                } else {
                    bail!("Could not determine compression format of kernel");
                }
            };

            if format == CompressFormat::UNKNOWN {
                std::io::copy(&mut kernel_source, output)?;
            } else {
                let mut encoder = get_encoder(format, output)?;
                std::io::copy(&mut kernel_source, encoder.deref_mut())?;
                encoder.finish()?;
            }

            pos = output.seek(SeekFrom::Current(0))?;
            pos - kernel_off
        } else {
            0
        };

        println!("kernel off {} sz {} pos {}", kernel_off, kernel_size, pos);

        file_align!();

        let ramdisk_off = pos;

        let (ramdisk_size, vendor_ramdisk_table) = if let Some(vendor_ramdisk_table) = self.source_boot_image.blocks.ramdisk.as_ref().and_then(|it| it.vendor_ramdisk_table.as_ref()) {
            if self.replace_ramdisk.is_some() {
                bail!("Could not replace ramdisk for vendor boot v4, please use replace_vendor_ramdisk!");
            }
            let mut vendor_ramdisk_table: Vec<VendorRamdiskEntry> = vendor_ramdisk_table.clone();

            if let Some((index, _)) = self.replace_vendor_ramdisk.iter().find(|(index, _)| {
                **index >= vendor_ramdisk_table.len()
            }) {
                bail!("invalid index {}", index);
            }

            for (index, entry) in vendor_ramdisk_table.iter_mut().enumerate() {
                let (mut ramdisk_source, compressed): (Box<dyn Read>, bool) = if let Some(payload) = self.replace_vendor_ramdisk.remove(&index) {
                    (payload.data, payload.compressed)
                } else {
                    (Box::new(entry.data), true)
                };
                let format = if compressed {
                    CompressFormat::UNKNOWN
                } else {
                    entry.compress_format
                };

                let entry_off = pos;
                entry.entry_offset = entry_off - ramdisk_off;

                if format == CompressFormat::UNKNOWN {
                    std::io::copy(&mut ramdisk_source, output)?;
                } else {
                    let mut encoder = get_encoder(format, output)?;
                    std::io::copy(&mut ramdisk_source, encoder.deref_mut())?;
                    encoder.finish()?;
                }

                pos = output.seek(SeekFrom::Current(0))?;
                entry.entry_size = pos - entry_off;
            }

            (pos - ramdisk_off, Some(vendor_ramdisk_table))
        } else {
            if !self.replace_vendor_ramdisk.is_empty() {
                bail!("Could not replace vendor ramdisk, please use replace_ramdisk!");
            }
            let ramdisk_source: Option<(Box<dyn Read>, bool)> = if let Some(payload) = self.replace_ramdisk {
                println!("using replace_ramdisk compressed={}", payload.compressed);
                Some((payload.data, payload.compressed))
            } else if let Some(ramdisk) = &self.source_boot_image.blocks.ramdisk {
                println!("using source ramdisk");
                Some((Box::new(ramdisk.data), true))
            } else {
                None
            };

            let ramdisk_size = if let Some((mut ramdisk_source, compressed)) = ramdisk_source {
                let format = if compressed {
                    CompressFormat::UNKNOWN
                } else {
                    if let Some(orig) = &self.source_boot_image.blocks.ramdisk {
                        orig.compress_format
                    } else {
                        bail!("Could not determine compression format of ramdisk");
                    }
                };

                println!("new ramdisk format {:?}", format);

                if format == CompressFormat::UNKNOWN {
                    std::io::copy(&mut ramdisk_source, output)?;
                } else {
                    let mut encoder = get_encoder(format, output)?;
                    std::io::copy(&mut ramdisk_source, encoder.deref_mut())?;
                    encoder.finish()?;
                }

                pos = output.seek(SeekFrom::Current(0))?;
                pos - ramdisk_off
            } else {
                0
            };

            (ramdisk_size, None)
        };

        println!("ramdisk off {} sz {} pos {}", ramdisk_off, ramdisk_size, pos);

        file_align!();

        let second_size;
        let recovery_dtbo_size;
        let dtb_size;
        let signature_size;
        let bootconfig_size;

        macro_rules! copy_block {
            ($name:ident) => {
                paste! {
                    let [<$name _off>] = pos;
                    [<$name _size>] = if let Some(second) = self.source_boot_image.blocks.$name {
                        output.write_all(second)?;
                        pos = output.seek(SeekFrom::Current(0))?;
                        pos - [<$name _off>]
                    } else {
                        0
                    };
                    file_align!();
                }
            };
        }

        copy_block! { second }
        // TODO: extra
        copy_block! { recovery_dtbo }
        copy_block! { dtb }
        copy_block! { signature }

        let vendor_ramdisk_table_off = pos;
        let vendor_ramdisk_table_size = if let Some(vendor_ramdisk_table) = vendor_ramdisk_table {
            for entry in vendor_ramdisk_table {
                output.write_all(&entry.entry.patch(entry.entry_size as u32, entry.entry_offset as u32))?;
            }

            pos = output.seek(SeekFrom::Current(0))?;
            pos - vendor_ramdisk_table_off
        } else {
            0
        };

        copy_block! { bootconfig }

        // Copy and patch AVB

        if let Some(avb_info) = self.source_boot_image.avb_info.as_ref() {
            if let Some(avb_tail) = avb_info.avb_tail {
                output.write_all(avb_tail)?;
                pos = output.seek(SeekFrom::Current(0))?;
            }
            file_align!();

            let total_size = pos;
            file_align_with!(4096);
            let avb_header_off = pos;
            output.write_all(avb_info.avb_header)?;

            output.seek(SeekFrom::End(-(AvbFooter::SIZE as i64)))?;
            output.write_all(&avb_info.avb_footer.patch(total_size, avb_header_off))?;
        }

        // Patch header

        macro_rules! patch_size {
            ($name:ident) => {
                paste! {
                    if self.source_boot_image.header.layout.[<offset_ $name _size>] != 0 {
                        output.seek(SeekFrom::Start(header_off + self.source_boot_image.header.layout.[<offset_ $name _size>] as u64))?;
                        output.write_all(&([<$name _size>] as u32).to_le_bytes())?;
                    }
                }
            }
        }

        patch_size! { kernel }
        patch_size! { ramdisk }
        patch_size! { second }
        patch_size! { recovery_dtbo }
        patch_size! { dtb }
        patch_size! { signature }
        patch_size! { vendor_ramdisk_table }
        patch_size! { bootconfig }

        // TODO: id
        // TODO: AVB1
        // TODO: special headers

        output.flush()?;

        Ok(())
    }
}

impl BootImageOutput for File {
    fn truncate(&mut self, size: u64) -> std::io::Result<()> {
        self.set_len(size)
    }
}

fn trim_end(data: &[u8]) -> &[u8] {
    &data[..data.iter().position(|&b| b == 0).unwrap_or(data.len())]
}

fn main() -> Result<()> {
    if let Some(s) = env::args().skip(1).next() {
        let file = File::open(s)?;
        let mem = unsafe { Mmap::map(&file)? };
        let boot = BootImage::parse(&mem)?;

        println!("version: {:?}", boot.header.version);
        println!("layout: {:?}", boot.header.layout);
        boot.print_info()?;

        fn dump_block_to_file(block: &[u8], name: &str) -> Result<()> {
            let mut output = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(name)?;
            dump_block(block, &mut output, false)
        }

        if let Some(kernel) = &boot.blocks.kernel {
            println!("kernel format: {:?}", kernel.compress_format);
            dump_block_to_file(kernel.data, "kernel")?;
        }

        if let Some(ramdisk) = &boot.blocks.ramdisk {
            if let Some(table) = &ramdisk.vendor_ramdisk_table {
                println!("vendor ramdisk table");
                for t in table {
                    if let Ok(name) = from_utf8(trim_end(t.entry.get_ramdisk_name())) {
                        println!("name: {}", name);
                        println!("type: {:?}", t.entry.get_ramdisk_type());
                        dump_block_to_file(t.data, &format!("vendor.{}.cpio", name))?;
                    } else {
                        println!("invalid ramdisk name: {:?}", t.entry.get_ramdisk_name());
                    }
                }
            } else {
                println!("ramdisk format: {:?}", ramdisk.compress_format);
                dump_block_to_file(ramdisk.data, "ramdisk.cpio")?;
            }
        }

        if let Some(avb_info) = &boot.avb_info {
            println!("avb");
            if let Some(tail) = avb_info.avb_tail {
                println!("avb tail {}", tail.len());
            }
        }

        if let Some(s2) = env::args().skip(2).next() {
            if s2 == "--patch" {

                let mut patcher = BootImagePatchOption::new(&boot);
                if boot.blocks.kernel.is_some() {
                    println!("adding kernel");
                    patcher.replace_kernel(Box::new(File::open("kernel")?), false);
                }
                if boot.blocks.ramdisk.is_some() {
                    println!("adding ramdisk");
                    patcher.replace_ramdisk(Box::new(File::open("ramdisk.cpio")?), false);
                }
                let mut output = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open("new-boot.img")?;
                patcher.patch(&mut output)?;

            }
        }

        Ok(())
    } else {
        bail!("no file provided")
    }
}
