// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

/// This module implements a muxer for vsock - a mediator between guest-side
/// AF_VSOCK sockets and host-side backends. The heavy lifting is performed by
/// `muxer::VsockMuxer`, a connection multiplexer that uses
/// `super::csm::VsockConnection` for handling vsock connection states. Check
/// out `muxer.rs` for a more detailed explanation of the inner workings of this
/// backend.
pub mod muxer_impl;
pub mod muxer_killq;
pub mod muxer_rxq;

use super::backend::{VsockBackend, VsockBackendType};
use super::{VsockChannel, VsockEpollListener};

mod defs {
    /// Size of the muxer RX packet queue.
    pub const MUXER_RXQ_SIZE: usize = 256;

    /// Size of the muxer connection kill queue.
    pub const MUXER_KILLQ_SIZE: usize = 128;
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {}

/// The vsock generic muxer, which is basically an epoll-event-driven vsock
/// channel. Currently, the only implementation we have is
/// `vsock::muxer::muxer::VsockMuxer`, which translates guest-side vsock
/// connections to host-side connections with different backends.
pub trait VsockGenericMuxer: VsockChannel + VsockEpollListener + Send {
    fn add_backend(&mut self, backend: Box<dyn VsockBackend>, is_peer_backend: bool) -> Result<()>;
}
