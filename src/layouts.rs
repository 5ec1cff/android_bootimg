use anyhow::{bail, Result};
use paste::paste;

use crate::constants::{BOOT_ARGS_SIZE, BOOT_EXTRA_ARGS_SIZE, BOOT_ID_SIZE, BOOT_NAME_SIZE, VENDOR_BOOT_ARGS_SIZE};

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
            impl BootHeaderLayout {
                $(
                    pub fn [<has_ $name>](&self) -> bool {
                        return self.[<offset_ $name>] != 0;
                    }
                    pub fn [<get_ $name _unchecked>](&self, arr: &[u8]) -> $t {
                        let offset = self.[<offset_ $name>] as usize;
                        return $t::from_le_bytes(arr[offset..offset + 4].try_into().unwrap());
                    }
                    pub fn [<get_ $name>](&self, arr: &[u8]) -> Result<$t> {
                        let offset = self.[<offset_ $name>] as usize;
                        if offset != 0 {
                            if let Some(data) = arr.get(offset..offset + 4) {
                                return Ok($t::from_le_bytes(data.try_into()?));
                            }
                        } else {
                            bail!("Use undefined field {}", stringify!($name));
                        }

                        bail!("Invalid offset 0x{:08x} field {}", offset, stringify!($name))
                    }
                )+
                $(
                    pub fn [<has_ $name2>](&self) -> bool {
                        return self.[<offset_ $name2>] != 0;
                    }
                    pub fn [<get_ $name2 _unchecked>]<'a>(&self, arr: &'a [u8]) -> &'a [u8] {
                        let offset = self.[<offset_ $name2>] as usize;
                        let sz = self.[<size_ $name2>] as usize;
                        return &arr[offset..offset + sz];
                    }
                    pub fn [<get_ $name2>]<'a>(&self, arr: &'a [u8]) -> Result<&'a [u8]> {
                        let offset = self.[<offset_ $name2>] as usize;
                        let sz = self.[<size_ $name2>] as usize;
                        if offset != 0 {
                            if let Some(data) = arr.get(offset..offset + sz) {
                                return Ok(data);
                            }
                        } else {
                            bail!("Use undefined field {}", stringify!($name2));
                        }
                        bail!("Invalid offset 0x{:08x} field {}", offset, stringify!($name2))
                    }
                )+

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
            const [<size_ $name>]: usize = $sz;
        }
    };
}

macro_rules! define_layout_offsets {
    ($name:ident $t:tt $(,)?) => {
        paste! {
            struct_item_maybe_def_size! { $name $t }
            const total_size: usize = [<offset_ $name>] + struct_item_size! { $t };
        }
    };
    ($name1:ident $t1:tt, $name2:ident $t2:tt $(,)?) => {
        paste! {
            struct_item_maybe_def_size! { $name1 $t1 }
            const [<offset_ $name2>]: usize = [<offset_ $name1>] + struct_item_size! { $t1 };
            struct_item_maybe_def_size! { $name2 $t2 }
            const total_size: usize = [<offset_ $name2>] + struct_item_size! { $t2 };
        }
    };
    ($name1:ident $t1:tt, $name2:ident $t2:tt $(,$name:ident $t:tt)+ $(,)?) => {
        paste! {
            struct_item_maybe_def_size! { $name1 $t1 }
            const [<offset_ $name2>]: usize = [<offset_ $name1>] + struct_item_size! { $t1 };
            define_layout_offsets! { $name2 $t2, $($name $t),+ }
        }
    };
}

macro_rules! define_layout_internal {
    (
        $struct_name:ident,
        initial_offset $initial_offset:tt,
        default_layout $default_layout:ident,
        structure {$name1:ident $t1:tt $(,$name:ident $t:tt)* $(,)?},
        ifields {$($ifield:ident),* $(,)?},
        sfields {$($sfield:ident),* $(,)?}$(,)?
    ) => {
        paste! {
            #[allow(dead_code, non_upper_case_globals, nonstandard_style, unused)]
            mod [<mod_ $struct_name>] {
                use super::*;
                const [<offset_ $name1>]: usize = $initial_offset;
                define_layout_offsets!{$name1 $t1, $($name $t),*}

                pub const layout: BootHeaderLayout = BootHeaderLayout {
                    name: stringify!($struct_name),
                    $(
                        [<offset_ $ifield>]: [<offset_ $ifield>] as u16,
                    )*
                    $(
                        [<offset_ $sfield>]: [<offset_ $sfield>] as u16,
                        [<size_ $sfield>]: [<size_ $sfield>] as u16,
                    )*
                    total_size: total_size as u16,
                    ..$default_layout
                };
            }

            #[allow(unused)]
            pub use [<mod_ $struct_name>]::layout as $struct_name;
        }
    };
}

macro_rules! define_layout {
    (
        $struct_name:ident,
        structure {$($name:ident $t:tt),+ $(,)?},
        ifields {$($ifield:ident),* $(,)?},
        sfields {$($sfield:ident),* $(,)?}$(,)?
    ) => {
        define_layout_internal! {
            $struct_name,
            initial_offset 8,
            default_layout DEFAULT_LAYOUT,
            structure { $($name $t),+ },
            ifields { $($ifield),* },
            sfields { $($sfield),* },
        }
    };
}

macro_rules! define_layout_inherits {
    (
        $struct_name:ident,
        $inherited_name:ident,
        structure {$($name:ident $t:tt),+ $(,)?},
        ifields {$($ifield:ident),* $(,)?},
        sfields {$($sfield:ident),* $(,)?}$(,)?
    ) => {
        define_layout_internal! {
            $struct_name,
            initial_offset ($inherited_name.total_size as usize),
            default_layout $inherited_name,
            structure { $($name $t),+ },
            ifields { $($ifield),* },
            sfields { $($sfield),* },
        }
    }
}

define_layout! {
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

define_layout_inherits! {
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

define_layout_inherits! {
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

define_layout! {
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

define_layout_inherits! {
    BOOT_HEADER_V4, BOOT_HEADER_V3,
    structure {
        signature_size u32,
    },
    ifields {
        signature_size,
    },
    sfields {}
}


define_layout! {
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
        ramdisk_size,
        header_version,
    },
    sfields {
        cmdline,
    },
}

define_layout_inherits! {
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
