use paste::paste;

use crate::constants::{BOOT_ARGS_SIZE, BOOT_EXTRA_ARGS_SIZE, BOOT_ID_SIZE, BOOT_NAME_SIZE, VENDOR_BOOT_ARGS_SIZE, VENDOR_RAMDISK_NAME_SIZE, VENDOR_RAMDISK_TABLE_ENTRY_BOARD_ID_SIZE};

macro_rules! def_boot_header_layout {
    ({$($name:ident $t:ident),+ $(,)?}, {$($name2:ident),+ $(,)?}) => {
        paste! {
            #[allow(unused)]
            #[derive(Debug)]
            pub struct BootHeaderLayout {
                pub name: &'static str,
                $(
                    pub [<offset_ $name>]: u16,
                )+
                $(
                    pub [<offset_ $name2>]: u16,
                    pub [<size_ $name2>]: u16,
                )+
                pub total_size: u16
            }

            #[allow(unused)]
            const DEFAULT_LAYOUT: BootHeaderLayout = BootHeaderLayout {
                name: "default",
                $(
                    [<offset_ $name>]: 0,
                )+
                $(
                    [<offset_ $name2>]: 0,
                    [<size_ $name2>]: 0,
                )+
                total_size: 0
            };
        }
    };
}

def_boot_header_layout! {
    {
        kernel_size u32,
        ramdisk_size u32,
        second_size u32,
        page_size u32,
        header_version u32,
        // extra_size u32,
        os_version u32,

        // v1/v2 specific
        recovery_dtbo_size u32,
        recovery_dtbo_offset u64,
        header_size u32,
        dtb_size u32,

        // v4 specific
        signature_size u32,

        // v4 vendor specific
        vendor_ramdisk_table_size u32,
        vendor_ramdisk_table_entry_num u32,
        vendor_ramdisk_table_entry_size u32,
        bootconfig_size u32,
    },
    {
        name,
        cmdline,
        id,
        extra_cmdline,
    }
}

macro_rules! struct_item_size {
    (u32) => { 4 };
    (u64) => { 8 };
    ($sz:expr) => { $sz };
}

macro_rules! struct_item_maybe_def_size {
    ($name:ident u32) => {};
    ($name:ident u64) => {};
    ($name:ident $sz:expr) => {
        paste! {
            pub(super) const [<size_ $name>]: usize = $sz;
        }
    };
}

macro_rules! define_layout_offsets {
    ($name:ident $t:tt $(,)?) => {
        paste! {
            struct_item_maybe_def_size! { $name $t }
            pub(super) const total_size: usize = [<offset_ $name>] + struct_item_size! { $t };
        }
    };
    ($name1:ident $t1:tt, $name2:ident $t2:tt $(,)?) => {
        paste! {
            struct_item_maybe_def_size! { $name1 $t1 }
            pub(super) const [<offset_ $name2>]: usize = [<offset_ $name1>] + struct_item_size! { $t1 };
            struct_item_maybe_def_size! { $name2 $t2 }
            pub(super) const total_size: usize = [<offset_ $name2>] + struct_item_size! { $t2 };
        }
    };
    ($name1:ident $t1:tt, $name2:ident $t2:tt $(,$name:ident $t:tt)+ $(,)?) => {
        paste! {
            struct_item_maybe_def_size! { $name1 $t1 }
            pub(super) const [<offset_ $name2>]: usize = [<offset_ $name1>] + struct_item_size! { $t1 };
            define_layout_offsets! { $name2 $t2, $($name $t),+ }
        }
    };
}

macro_rules! define_layout_common {
    (
        $struct_name:ident,
        initial_offset $initial_offset:tt,
        structure {$name1:ident $t1:tt $(,$name:ident $t:tt)* $(,)?} $(,)?
    ) => {
        paste! {
            #[allow(dead_code, non_upper_case_globals, nonstandard_style, unused)]
            mod [<mod_offsets_ $struct_name>] {
                use super::*;
                pub(super) const [<offset_ $name1>]: usize = $initial_offset;
                define_layout_offsets!{$name1 $t1, $($name $t),*}

            }
        }
    };
}

macro_rules! define_boot_header_layout_common {
    (
        $struct_name:ident,
        initial_offset $initial_offset:tt,
        default_layout $default_layout:ident,
        structure {$($name:ident $t:tt),+ $(,)?},
        ifields {$($ifield:ident),* $(,)?},
        sfields {$($sfield:ident),* $(,)?}$(,)?
    ) => {
        define_layout_common! {
            $struct_name,
            initial_offset $initial_offset,
            structure { $($name $t),+ }
        }
        paste! {
            pub const $struct_name: BootHeaderLayout = BootHeaderLayout {
                name: stringify!($struct_name),
                $(
                    [<offset_ $ifield>]: [<mod_offsets_ $struct_name>]::[<offset_ $ifield>] as u16,
                )*
                $(
                    [<offset_ $sfield>]: [<mod_offsets_ $struct_name>]::[<offset_ $sfield>] as u16,
                    [<size_ $sfield>]: [<mod_offsets_ $struct_name>]::[<size_ $sfield>] as u16,
                )*
                total_size: [<mod_offsets_ $struct_name>]::total_size as u16,
                ..$default_layout
            };
        }
    };
}

macro_rules! define_boot_header_layout {
    (
        $struct_name:ident,
        structure {$($name:ident $t:tt),+ $(,)?},
        ifields {$($ifield:ident),* $(,)?},
        sfields {$($sfield:ident),* $(,)?}$(,)?
    ) => {
        define_boot_header_layout_common! {
            $struct_name,
            initial_offset 8,
            default_layout DEFAULT_LAYOUT,
            structure { $($name $t),+ },
            ifields { $($ifield),* },
            sfields { $($sfield),* },
        }
    };
}

macro_rules! define_boot_header_layout_inherits {
    (
        $struct_name:ident,
        $inherited_name:ident,
        structure {$($name:ident $t:tt),+ $(,)?},
        ifields {$($ifield:ident),* $(,)?},
        sfields {$($sfield:ident),* $(,)?}$(,)?
    ) => {
        define_boot_header_layout_common! {
            $struct_name,
            initial_offset ($inherited_name.total_size as usize),
            default_layout $inherited_name,
            structure { $($name $t),+ },
            ifields { $($ifield),* },
            sfields { $($sfield),* },
        }
    }
}

define_boot_header_layout! {
    BOOT_HEADER_V0,
    structure {
        kernel_size u32,
        kernel_addr u32,
        ramdisk_size u32,
        ramdisk_addr u32,
        second_size u32,
        second_addr u32,
        tags_addr u32,
        page_size u32,
        header_version u32,
        os_version u32,
        name BOOT_NAME_SIZE,
        cmdline BOOT_ARGS_SIZE,
        id BOOT_ID_SIZE,
        extra_cmdline BOOT_EXTRA_ARGS_SIZE,
    },
    ifields {
        kernel_size,
        ramdisk_size,
        second_size,
        page_size,
        header_version,
        os_version
    },
    sfields {
        name,
        cmdline,
        id,
    },
}

define_boot_header_layout_inherits! {
    BOOT_HEADER_V1, BOOT_HEADER_V0,
    structure {
        recovery_dtbo_size u32,
        recovery_dtbo_offset u64,
        header_size u32,
    },
    ifields {
        recovery_dtbo_size,
        recovery_dtbo_offset,
        header_size,
    },
    sfields {}
}

define_boot_header_layout_inherits! {
    BOOT_HEADER_V2, BOOT_HEADER_V1,
    structure {
        dtb_size u32,
        dtb_addr u64,
    },
    ifields {
        dtb_size,
    },
    sfields {}
}

define_boot_header_layout! {
    BOOT_HEADER_V3,
    structure {
        kernel_size u32,
        ramdisk_size u32,
        os_version u32,
        header_size u32,
        reserved 16,
        header_version u32,
        cmdline (BOOT_ARGS_SIZE + BOOT_EXTRA_ARGS_SIZE),
    },
    ifields {
        kernel_size,
        ramdisk_size,
        header_version,
        os_version,
    },
    sfields {
        cmdline,
    },
}

define_boot_header_layout_inherits! {
    BOOT_HEADER_V4, BOOT_HEADER_V3,
    structure {
        signature_size u32,
    },
    ifields {
        signature_size,
    },
    sfields {}
}


define_boot_header_layout! {
    VENDOR_BOOT_HEADER_V3,
    structure {
        header_version u32,
        page_size u32,
        kernel_addr u32,
        ramdisk_addr u32,
        ramdisk_size u32,
        cmdline VENDOR_BOOT_ARGS_SIZE,
        tags_addr u32,
        name BOOT_NAME_SIZE,
        header_size u32,
        dtb_size u32,
        dtb_addr u64,
    },
    ifields {
        page_size,
        ramdisk_size,
        header_version,
    },
    sfields {
        cmdline,
    },
}

define_boot_header_layout_inherits! {
    VENDOR_BOOT_HEADER_V4, VENDOR_BOOT_HEADER_V3,
    structure {
        vendor_ramdisk_table_size u32,
        vendor_ramdisk_table_entry_num u32,
        vendor_ramdisk_table_entry_size u32,
        bootconfig_size u32,
    },
    ifields {
        vendor_ramdisk_table_size,
        vendor_ramdisk_table_entry_num,
        vendor_ramdisk_table_entry_size,
        bootconfig_size,
    },
    sfields {}
}

macro_rules! impl_ifield_accessor {
    ($vis:vis, $mod_name:ident, $t:ty, $name:ident $(,$suffix:ident)?) => {
        paste! {
            #[allow(unused)]
            $vis fn [<get_ $name $($suffix)?>](&self) -> $t {
                let offset = [<mod_offsets_ $mod_name>]::[<offset_ $name>] as usize;
                return $t::from_le_bytes(self.data[offset..offset + size_of::<$t>()].try_into().unwrap());
            }
        }
    };
}

macro_rules! impl_sfield_accessor {
    ($vis:vis, $mod_name:ident, $name:ident $(,$suffix:ident)?) => {
        paste! {
            #[allow(unused)]
            $vis fn [<get_ $name $($suffix)?>](&self) -> &[u8] {
                let offset = [<mod_offsets_ $mod_name>]::[<offset_ $name>] as usize;
                let sz = [<mod_offsets_ $mod_name>]::[<size_ $name>] as usize;
                return &self.data[offset..offset + sz];
            }
        }
    };
}

define_layout_common! {
    VendorRamdiskTableEntryV4,
    initial_offset 0,
    structure {
        ramdisk_size u32,
        ramdisk_offset u32,
        ramdisk_type u32,
        ramdisk_name VENDOR_RAMDISK_NAME_SIZE,
        board_id (VENDOR_RAMDISK_TABLE_ENTRY_BOARD_ID_SIZE * size_of::<u32>()),
    },
}

pub struct VendorRamdiskTableEntryV4<'a> {
    pub data: &'a [u8],
}

#[derive(Debug)]
pub enum VendorRamdiskTableEntryType {
    None,
    Platform,
    Recovery,
    Unknown(u32),
}

impl VendorRamdiskTableEntryV4<'_> {
    impl_ifield_accessor! { pub, VendorRamdiskTableEntryV4, u32, ramdisk_size }
    impl_ifield_accessor! { pub, VendorRamdiskTableEntryV4, u32, ramdisk_offset }
    impl_ifield_accessor! { pub, VendorRamdiskTableEntryV4, u32, ramdisk_type, _raw }
    impl_sfield_accessor! { pub, VendorRamdiskTableEntryV4, ramdisk_name }
    impl_sfield_accessor! { pub, VendorRamdiskTableEntryV4, board_id }

    pub const SIZE: usize = mod_offsets_VendorRamdiskTableEntryV4::total_size;

    pub fn get_ramdisk_type(&self) -> VendorRamdiskTableEntryType {
        let raw = self.get_ramdisk_type_raw();
        match raw {
            0 => VendorRamdiskTableEntryType::None,
            1 => VendorRamdiskTableEntryType::Platform,
            2 => VendorRamdiskTableEntryType::Recovery,
            _ => VendorRamdiskTableEntryType::Unknown(raw),
        }
    }
}
