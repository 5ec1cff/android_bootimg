use crate::compress::{CompressFormat, get_decoder, parse_compress_format};
use crate::constants::{AVB_FOOTER_MAGIC, AVB_MAGIC};
use crate::layouts::{
    AvbFooter, BOOT_HEADER_V0, BOOT_HEADER_V1, BOOT_HEADER_V2, BOOT_HEADER_V3, BOOT_HEADER_V4,
    BootHeaderLayout, VENDOR_BOOT_HEADER_V3, VENDOR_BOOT_HEADER_V4, VendorRamdiskTableEntryType,
    VendorRamdiskTableEntryV4,
};
use crate::parser::BootImageVersion::{Android, Vendor};
use crate::utils::{SliceExt, align_to, trim_end};
use anyhow::bail;
use paste::paste;
use std::fmt::{Display, Formatter};
use std::io::Write;
use std::slice::Iter;
use std::str::from_utf8;

const BOOT_MAGIC: &[u8] = b"ANDROID!";
const VENDOR_BOOT_MAGIC: &[u8] = b"VNDRBOOT";

pub struct OsVersion {
    a: u32,
    b: u32,
    c: u32,
}

impl Display for OsVersion {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("{}.{}.{}", self.a, self.b, self.c))
    }
}

pub struct PatchLevel {
    year: u32,
    month: u32,
}

impl Display for PatchLevel {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("{}-{}", self.year, self.month))
    }
}

#[derive(Debug, Copy, Clone)]
pub enum BootImageVersion {
    Android(u32),
    Vendor(u32),
}

pub struct BootHeader<'a> {
    pub(crate) data: &'a [u8],
    pub(crate) layout: &'static BootHeaderLayout,
    pub(crate) version: BootImageVersion,
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

    pub fn get_layout(&self) -> &'static BootHeaderLayout {
        self.layout
    }

    pub fn get_version(&self) -> BootImageVersion {
        self.version
    }

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
            }
            _ => {}
        }

        self.get_page_size() as usize
    }

    pub fn hdr_space(&self) -> usize {
        // TODO: only vendor boot has page count > 1
        println!("ver: {:?}", self.version);
        align_to(self.layout.total_size as usize, self.page_size())
    }

    pub fn parse(data: &'a [u8]) -> anyhow::Result<Self> {
        if data.starts_with(BOOT_MAGIC) {
            if let Some(version) = data.u32_at(BOOT_HEADER_V0.offset_header_version as usize) {
                let layout = match version {
                    0 => &BOOT_HEADER_V0,
                    1 => &BOOT_HEADER_V1,
                    2 => &BOOT_HEADER_V2,
                    3 => &BOOT_HEADER_V3,
                    4 => &BOOT_HEADER_V4,
                    _ => bail!("unsupported boot version {}", version),
                };

                let data = &data[..layout.total_size as usize];

                return Ok(Self {
                    data,
                    layout,
                    version: Android(version),
                });
            }
        } else if data.starts_with(VENDOR_BOOT_MAGIC) {
            if let Some(version) = data.u32_at(VENDOR_BOOT_HEADER_V3.offset_header_version as usize)
            {
                let layout = match version {
                    3 => &VENDOR_BOOT_HEADER_V3,
                    4 => &VENDOR_BOOT_HEADER_V4,
                    _ => bail!("unsupported vendor boot version {}", version),
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

pub struct KernelImage<'a> {
    pub(crate) data: &'a [u8],
    pub(crate) compress_format: CompressFormat,
}

impl KernelImage<'_> {
    pub fn get_data(&self) -> &[u8] {
        self.data
    }

    pub fn get_compress_format(&self) -> CompressFormat {
        self.compress_format
    }

    pub fn dump(&self, out: &mut dyn Write, raw: bool) -> anyhow::Result<()> {
        dump_block(self.data, out, raw)
    }
}

pub struct RamdiskImage<'a> {
    pub(crate) data: &'a [u8],
    pub(crate) compress_format: CompressFormat,
    pub(crate) vendor_ramdisk_table: Option<Vec<VendorRamdiskEntry<'a>>>,
}

impl RamdiskImage<'_> {
    pub fn get_data(&self) -> &[u8] {
        self.data
    }

    pub fn get_compress_format(&self) -> CompressFormat {
        self.compress_format
    }

    pub fn dump(&self, out: &mut dyn Write, raw: bool) -> anyhow::Result<()> {
        if self.vendor_ramdisk_table.is_some() {
            bail!("")
        }
        dump_block(self.data, out, raw)
    }

    pub fn is_vendor_ramdisk(&self) -> bool {
        self.vendor_ramdisk_table.is_some()
    }

    pub fn get_vendor_ramdisk_num(&self) -> usize {
        self.vendor_ramdisk_table
            .as_ref()
            .map(|t| t.len())
            .unwrap_or(0)
    }

    pub fn get_vendor_ramdisk(&self, index: usize) -> Option<&VendorRamdiskEntry<'_>> {
        self.vendor_ramdisk_table
            .as_ref()
            .and_then(|t| t.get(index))
    }

    pub fn iter_vendor_ramdisk(&self) -> Iter<'_, VendorRamdiskEntry<'_>> {
        self.vendor_ramdisk_table
            .as_ref()
            .map(|v| v.iter())
            .unwrap_or_default()
    }
}

pub struct BootImageBlocks<'a> {
    pub(crate) kernel: Option<KernelImage<'a>>,
    pub(crate) ramdisk: Option<RamdiskImage<'a>>,
    pub(crate) second: Option<&'a [u8]>,
    // TODO: extra
    pub(crate) recovery_dtbo: Option<&'a [u8]>,
    pub(crate) dtb: Option<&'a [u8]>,
    pub(crate) signature: Option<&'a [u8]>,
    pub(crate) bootconfig: Option<&'a [u8]>,
}

impl<'a> BootImageBlocks<'a> {
    pub fn get_kernel(&self) -> Option<&KernelImage<'a>> {
        self.kernel.as_ref()
    }

    pub fn get_ramdisk(&self) -> Option<&RamdiskImage<'a>> {
        self.ramdisk.as_ref()
    }

    pub fn parse(data: &'a [u8], boot_header: &BootHeader) -> anyhow::Result<(Self, usize)> {
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
                compress_format: parse_compress_format(data),
            })
        } else {
            None
        };

        let vendor_ramdisk_table = if let Some(entry_table) = &vendor_ramdisk_table {
            let entry_size = boot_header.get_vendor_ramdisk_table_entry_size() as usize;
            if entry_size != VendorRamdiskTableEntryV4::SIZE {
                bail!("invalid vendor ramdisk table entry size: {}", entry_size);
            }

            let entry_table_size =
                boot_header.get_vendor_ramdisk_table_entry_num() as usize * entry_size;

            if entry_table.len() < entry_table_size {
                bail!(
                    "invalid vendor ramdisk table entry size: {}",
                    entry_table.len()
                );
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
                vendor_ramdisk_table,
            })
        } else {
            None
        };

        Ok((
            BootImageBlocks {
                kernel,
                ramdisk,
                second,
                recovery_dtbo,
                dtb,
                signature,
                bootconfig,
            },
            off,
        ))
    }
}

pub(crate) struct BootImageAVBInfo<'a> {
    pub(crate) avb_tail: Option<&'a [u8]>,
    pub(crate) avb_header: &'a [u8],
    pub(crate) avb_footer: AvbFooter<'a>,
}

#[derive(Copy, Clone)]
pub struct VendorRamdiskEntry<'a> {
    pub(crate) data: &'a [u8],
    pub(crate) entry_offset: u64,
    pub(crate) entry_size: u64,
    pub(crate) entry_type: VendorRamdiskTableEntryType,
    pub(crate) compress_format: CompressFormat,
    pub(crate) entry: VendorRamdiskTableEntryV4<'a>,
}

impl VendorRamdiskEntry<'_> {
    pub fn get_data(&self) -> &[u8] {
        self.data
    }

    pub fn get_name_raw(&self) -> &[u8] {
        trim_end(self.entry.get_ramdisk_name())
    }

    pub fn get_name(&self) -> anyhow::Result<&str> {
        Ok(from_utf8(trim_end(self.entry.get_ramdisk_name()))?)
    }

    pub fn get_entry_type(&self) -> VendorRamdiskTableEntryType {
        self.entry_type
    }

    pub fn get_compress_format(&self) -> CompressFormat {
        self.compress_format
    }

    pub fn dump(&self, out: &mut dyn Write, raw: bool) -> anyhow::Result<()> {
        dump_block(self.data, out, raw)
    }
}

pub struct BootImage<'a> {
    pub(crate) data: &'a [u8],
    pub(crate) header: BootHeader<'a>,
    pub(crate) blocks: BootImageBlocks<'a>,
    pub(crate) avb_info: Option<BootImageAVBInfo<'a>>,
}

fn dump_block(data: &[u8], out: &mut dyn Write, raw: bool) -> anyhow::Result<()> {
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
    pub fn parse(data: &'a [u8]) -> anyhow::Result<Self> {
        let header = BootHeader::parse(data)?;
        let (blocks, tail) = BootImageBlocks::parse(data, &header)?;

        let avb_info = if let Some(avb_footer) = data.get(data.len() - AvbFooter::SIZE..) {
            if avb_footer.starts_with(AVB_FOOTER_MAGIC) {
                let avb_footer = AvbFooter { data: avb_footer };
                let off = avb_footer.get_vbmeta_offset() as usize;
                if let Some(avb_header) = data.get(off..off + avb_footer.get_vbmeta_size() as usize)
                {
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

        Ok(Self {
            data,
            header,
            blocks,
            avb_info,
        })
    }

    pub fn get_header(&self) -> &BootHeader<'_> {
        &self.header
    }

    pub fn get_blocks(&self) -> &BootImageBlocks<'_> {
        &self.blocks
    }
}
