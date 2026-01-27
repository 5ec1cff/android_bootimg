use android_bootimg::{parser::BootHeader, parser::BootImage, patcher::BootImagePatchOption};
use anyhow::{Result, bail};
use memmap2::Mmap;
use paste::paste;
use std::env;
use std::fs::{File, OpenOptions};
use std::str::from_utf8;

fn print_info(header: &BootHeader) -> Result<()> {
    macro_rules! print_info_item {
        ($name:ident) => {
            paste! {
                if header.[<has_ $name>]() {
                    let d = header.[<get_ $name>]();
                    println!("{}: {}", stringify!($name), d);
                }
            }
        };
    }

    print_info_item! { kernel_size }
    print_info_item! { ramdisk_size }
    print_info_item! { second_size }
    print_info_item! { page_size }
    print_info_item! { header_version }
    if header.has_os_version_raw() {
        if let Some((os_version, patch_level)) = header.get_os_version() {
            println!("os_version: {}", os_version);
            println!("patch_level: {}", patch_level);
        }
    }
    print_info_item! { recovery_dtbo_size }
    print_info_item! { recovery_dtbo_offset }
    print_info_item! { header_size }
    print_info_item! { dtb_size }

    print_info_item! { signature_size }

    print_info_item! { vendor_ramdisk_table_size }
    print_info_item! { vendor_ramdisk_table_entry_num }
    print_info_item! { vendor_ramdisk_table_entry_size }
    print_info_item! { bootconfig_size }

    Ok(())
}

fn main() -> Result<()> {
    if let Some(s) = env::args().skip(1).next() {
        let file = File::open(s)?;
        let mem = unsafe { Mmap::map(&file)? };
        let boot = BootImage::parse(&mem)?;

        let header = boot.get_header();

        println!("version: {:?}", header.get_version());
        println!("layout: {:?}", header.get_layout());
        print_info(header)?;

        macro_rules! dump_block_to_file {
            ($block:ident, $filename:expr) => {
                let mut output = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open($filename)?;
                $block.dump(&mut output, false)?
            };
        }

        let blocks = boot.get_blocks();

        if let Some(kernel) = blocks.get_kernel() {
            println!("kernel format: {:?}", kernel.get_compress_format());
            dump_block_to_file!(kernel, "kernel");
        }

        if let Some(ramdisk) = blocks.get_ramdisk() {
            if ramdisk.is_vendor_ramdisk() {
                println!("vendor ramdisk table");
                for i in 0..ramdisk.get_vendor_ramdisk_num() {
                    let entry = ramdisk.get_vendor_ramdisk(i).unwrap();
                    if let Ok(name) = from_utf8(entry.get_name_raw()) {
                        println!("name: {}", name);
                        println!("type: {:?}", entry.get_entry_type());
                        dump_block_to_file!(entry, &format!("vendor.{}.cpio", name));
                        let mut data = Vec::<u8>::new();
                        entry.dump(&mut data, false)?;
                        let cpio = android_bootimg::cpio::Cpio::load_from_data(data.as_slice())?;
                        cpio.ls("/", true);
                    } else {
                        println!("invalid ramdisk name: {:?}", entry.get_name_raw());
                    }
                }
            } else {
                println!("ramdisk format: {:?}", ramdisk.get_compress_format());
                dump_block_to_file!(ramdisk, "ramdisk.cpio");
                let mut data = Vec::<u8>::new();
                ramdisk.dump(&mut data, false)?;
                let cpio = android_bootimg::cpio::Cpio::load_from_data(data.as_slice())?;
                cpio.ls("/", true);
            }
        }

        if let Some(s2) = env::args().skip(2).next() {
            if s2 == "--patch" {
                let mut patcher = BootImagePatchOption::new(&boot);
                if blocks.get_kernel().is_some() {
                    println!("adding kernel");
                    patcher.replace_kernel(Box::new(File::open("kernel")?), false);
                }
                if let Some(ramdisk) = blocks.get_ramdisk() {
                    if ramdisk.is_vendor_ramdisk() {
                        println!("adding vendor ramdisk");
                        for i in 0..ramdisk.get_vendor_ramdisk_num() {
                            let entry = ramdisk.get_vendor_ramdisk(i).unwrap();
                            let name = from_utf8(entry.get_name_raw())?;
                            println!("name: {}", name);
                            patcher.replace_vendor_ramdisk(
                                i,
                                Box::new(File::open(format!("vendor.{}.cpio", name))?),
                                false,
                            );
                        }
                    } else {
                        println!("adding ramdisk");
                        patcher.replace_ramdisk(Box::new(File::open("ramdisk.cpio")?), false);
                    }
                }
                // TODO: vendor ramdisk
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
