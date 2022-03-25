// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the THIRD-PARTY file.

pub mod backend;
mod packet;

use vm_memory::GuestMemoryError;

mod defs {
    /// Max vsock packet data/buffer size.
    pub const MAX_PKT_BUF_SIZE: usize = 64 * 1024;
}

#[derive(Debug, thiserror::Error)]
pub enum VsockError {
    /// vsock backend error
    #[error("Vsock backend error: {0}")]
    Backend(#[source] std::io::Error),
    /// The vsock data/buffer virtio descriptor is expected, but missing.
    #[error("The vsock data/buffer virtio descriptor is expected, but missing")]
    BufDescMissing,
    /// The vsock data/buffer virtio descriptor length is smaller than expected.
    #[error("The vsock data/buffer virtio descriptor length is smaller than expected")]
    BufDescTooSmall,
    /// Chained GuestMemory error.
    #[error("Chained GuestMemory error: {0}")]
    GuestMemory(#[source] GuestMemoryError),
    /// Bounds check failed on guest memory pointer.
    #[error("Bounds check failed on guest memory pointer, addr: {0}, size: {1}")]
    GuestMemoryBounds(u64, usize),
    /// The vsock header descriptor length is too small.
    #[error("The vsock header descriptor length {0} is too small")]
    HdrDescTooSmall(u32),
    /// The vsock header `len` field holds an invalid value.
    #[error("The vsock header `len` field holds an invalid value {0}")]
    InvalidPktLen(u32),
    /// Encountered an unexpected write-only virtio descriptor.
    #[error("Encountered an unexpected write-only virtio descriptor")]
    UnreadableDescriptor,
    /// Encountered an unexpected read-only virtio descriptor.
    #[error("Encountered an unexpected read-only virtio descriptor")]
    UnwritableDescriptor,
}

type Result<T> = std::result::Result<T, VsockError>;
