use crate::compress::{CompressFormat, get_encoder};
use crate::layouts::AvbFooter;
use crate::parser::{BootImage, OsVersion, PatchLevel, VendorRamdiskEntry};
use crate::utils::align_to;
use anyhow::bail;
use paste::paste;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::ops::DerefMut;

struct ReplacePayload {
    data: Box<dyn Read>,
    compressed: bool,
}

pub struct BootImagePatchOption<'a> {
    source_boot_image: &'a BootImage<'a>,
    replace_ramdisk: Option<ReplacePayload>,
    replace_kernel: Option<ReplacePayload>,
    replace_vendor_ramdisk: HashMap<usize, ReplacePayload>,
    // TODO: allow replace other blocks
    override_cmdline: Option<&'a [u8]>,
    override_os_version: Option<(OsVersion, PatchLevel)>,
}

pub trait BootImageOutput: Read + Write + Seek {
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
        self.replace_ramdisk = Some(ReplacePayload {
            data: ramdisk,
            compressed,
        });
        self
    }

    pub fn replace_kernel(&mut self, kernel: Box<dyn Read>, compressed: bool) -> &mut Self {
        self.replace_kernel = Some(ReplacePayload {
            data: kernel,
            compressed,
        });
        self
    }

    pub fn replace_vendor_ramdisk(
        &mut self,
        index: usize,
        ramdisk: Box<dyn Read>,
        compressed: bool,
    ) -> &mut Self {
        self.replace_vendor_ramdisk.insert(
            index,
            ReplacePayload {
                data: ramdisk,
                compressed,
            },
        );
        self
    }

    pub fn override_cmdline(&mut self, override_cmdline: &'a [u8]) -> &mut Self {
        self.override_cmdline = Some(override_cmdline);
        self
    }

    pub fn override_os_version(
        &mut self,
        override_os_version: (OsVersion, PatchLevel),
    ) -> &mut Self {
        self.override_os_version = Some(override_os_version);
        self
    }

    pub fn patch(mut self, output: &mut dyn BootImageOutput) -> anyhow::Result<()> {
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
        output
            .write_all(&self.source_boot_image.data[..self.source_boot_image.header.hdr_space()])?;
        pos += self.source_boot_image.header.hdr_space() as u64;
        println!("header space {}", pos);

        let kernel_off = pos;
        let kernel_source: Option<(Box<dyn Read>, bool)> =
            if let Some(payload) = self.replace_kernel {
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

        let (ramdisk_size, vendor_ramdisk_table) = if let Some(vendor_ramdisk_table) = self
            .source_boot_image
            .blocks
            .ramdisk
            .as_ref()
            .and_then(|it| it.vendor_ramdisk_table.as_ref())
        {
            if self.replace_ramdisk.is_some() {
                bail!(
                    "Could not replace ramdisk for vendor boot v4, please use replace_vendor_ramdisk!"
                );
            }
            let mut vendor_ramdisk_table: Vec<VendorRamdiskEntry> = vendor_ramdisk_table.clone();

            if let Some((index, _)) = self
                .replace_vendor_ramdisk
                .iter()
                .find(|(index, _)| **index >= vendor_ramdisk_table.len())
            {
                bail!("invalid index {}", index);
            }

            for (index, entry) in vendor_ramdisk_table.iter_mut().enumerate() {
                let (mut ramdisk_source, compressed): (Box<dyn Read>, bool) =
                    if let Some(payload) = self.replace_vendor_ramdisk.remove(&index) {
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
            let ramdisk_source: Option<(Box<dyn Read>, bool)> =
                if let Some(payload) = self.replace_ramdisk {
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

        println!(
            "ramdisk off {} sz {} pos {}",
            ramdisk_off, ramdisk_size, pos
        );

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
                output.write_all(
                    &entry
                        .entry
                        .patch(entry.entry_size as u32, entry.entry_offset as u32),
                )?;
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
