// Copyright 2019 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//      ==== Address map in use in ARM development systems today ====
//
//              - 32-bit -              - 36-bit -          - 40-bit -
//1024GB    +                   +                      +-------------------+     <- 40-bit
//          |                                           | DRAM              |
//          ~                   ~                       ~                   ~
//          |                                           |                   |
//          |                                           |                   |
//          |                                           |                   |
//          |                                           |                   |
//544GB     +                   +                       +-------------------+
//          |                                           | Hole or DRAM      |
//          |                                           |                   |
//512GB     +                   +                       +-------------------+
//          |                                           |       Mapped      |
//          |                                           |       I/O         |
//          ~                   ~                       ~                   ~
//          |                                           |                   |
//256GB     +                   +                       +-------------------+
//          |                                           |       Reserved    |
//          ~                   ~                       ~                   ~
//          |                                           |                   |
//64GB      +                   +-----------------------+-------------------+   <- 36-bit
//          |                   |                   DRAM                    |
//          ~                   ~                   ~                       ~
//          |                   |                                           |
//          |                   |                                           |
//34GB      +                   +-----------------------+-------------------+
//          |                   |                  Hole or DRAM             |
//32GB      +                   +-----------------------+-------------------+
//          |                   |                   Mapped I/O              |
//          ~                   ~                       ~                   ~
//          |                   |                                           |
//16GB      +                   +-----------------------+-------------------+
//          |                   |                   Reserved                |
//          ~                   ~                       ~                   ~
//4GB       +-------------------+-----------------------+-------------------+   <- 32-bit
//          |           2GB of DRAM                                         |
//          |                                                               |
//2GB       +-------------------+-----------------------+-------------------+
//          |                           Mapped I/O                          |
//1GB       +-------------------+-----------------------+-------------------+
//          |                          ROM & RAM & I/O                      |
//0GB       +-------------------+-----------------------+-------------------+   0
//              - 32-bit -              - 36-bit -              - 40-bit -
//
// Taken from (http://infocenter.arm.com/help/topic/com.arm.doc.den0001c/DEN0001C_principles_of_arm_memory_maps.pdf).

/// Start of RAM on 64 bit ARM.
pub const DRAM_MEM_START: u64 = 0x8000_0000; // 2 GB.
/// The maximum addressable RAM address.
pub const DRAM_MEM_END: u64 = 0x00F8_0000_0000; // 1024 - 32 = 992 GB.
/// The maximum RAM size.
pub const DRAM_MEM_MAX_SIZE: u64 = DRAM_MEM_END - DRAM_MEM_START;

/// Kernel command line maximum size.
/// As per `arch/arm64/include/uapi/asm/setup.h`.
pub const CMDLINE_MAX_SIZE: usize = 2048;

/// Maximum size of the device tree blob as specified in https://www.kernel.org/doc/Documentation/arm64/booting.txt.
pub const FDT_MAX_SIZE: usize = 0x20_0000;

// As per virt/kvm/arm/vgic/vgic-kvm-device.c we need
// the number of interrupts our GIC will support to be:
// * bigger than 32
// * less than 1023 and
// * a multiple of 32.
// We are setting up our interrupt controller to support a maximum of 128 interrupts.
/// First usable interrupt on aarch64.
pub const IRQ_BASE: u32 = 32;

/// Last usable interrupt on aarch64.
pub const IRQ_MAX: u32 = 159;

/// Below this address will reside the GIC, above this address will reside the MMIO devices.
pub const MAPPED_IO_START: u64 = 1 << 30; // 1 GB
/// End address (inclusive) of the MMIO window.
pub const MAPPED_IO_END: u64 = (2 << 30) - 1; // 1 GB
