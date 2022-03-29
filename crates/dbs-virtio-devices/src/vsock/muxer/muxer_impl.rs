// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

// Portions Copyright (C) 2021 Alibaba Cloud Computing. All rights reserved.

/// `VsockMuxer` is the device-facing component of multiple vsock backends. You
/// can add various of backends to VsockMuxer which implements the
/// `VsockBackend` trait. VsockMuxer can abstracts away the gory details of
/// translating between AF_VSOCK and the protocol of backends which you added.
/// It can also presents a clean interface to the rest of the vsock device
/// model.
///
/// The vsock muxer has two main roles:
/// 1. Vsock connection multiplexer: It's the muxer's job to create, manage, and
///    terminate `VsockConnection` objects. The muxer also routes packets to
///    their owning connections. It does so via a connection `HashMap`, keyed by
///    what is basically a (host_port, guest_port) tuple. Vsock packet traffic
///    needs to be inspected, in order to detect connection request packets
///    (leading to the creation of a new connection), and connection reset
///    packets (leading to the termination of an existing connection). All other
///    packets, though, must belong to an existing connection and, as such, the
///    muxer simply forwards them.
/// 2. Event dispatcher There are three event categories that the vsock backend
///    is interested it:
///    1. A new host-initiated connection is ready to be accepted from the
///       backends added to muxer;
///    2. Data is available for reading from a newly-accepted host-initiated
///       connection (i.e. the host is ready to issue a vsock connection
///       request, informing us of the destination port to which it wants to
///       connect);
///    3. Some event was triggered for a connected backend connection, that
///       belongs to a `VsockConnection`. The muxer gets notified about all of
///       these events, because, as a `VsockEpollListener` implementor, it gets
///       to register a nested epoll FD into the main VMM epolling loop. All
///       other pollable FDs are then registered under this nested epoll FD. To
///       route all these events to their handlers, the muxer uses another
///       `HashMap` object, mapping `RawFd`s to `EpollListener`s.

/// A unique identifier of a `VsockConnection` object. Connections are stored in
/// a hash map, keyed by a `ConnMapKey` object.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ConnMapKey {
    local_port: u32,
    peer_port: u32,
}

/// A muxer RX queue item.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MuxerRx {
    /// The packet must be fetched from the connection identified by
    /// `ConnMapKey`.
    ConnRx(ConnMapKey),
    /// The muxer must produce an RST packet.
    RstPkt { local_port: u32, peer_port: u32 },
}
