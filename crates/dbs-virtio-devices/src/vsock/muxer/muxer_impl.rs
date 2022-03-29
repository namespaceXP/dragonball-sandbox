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
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::os::unix::io::{AsRawFd, RawFd};

use log::{debug, error, info, trace, warn};

use super::super::backend::{VsockBackend, VsockBackendType, VsockStream};
use super::super::csm::{ConnState, VsockConnection};
use super::super::defs::uapi;
use super::super::packet::VsockPacket;
use super::super::{Result as VsockResult, VsockChannel, VsockEpollListener, VsockError};
use super::muxer_killq::MuxerKillQ;
use super::muxer_rxq::MuxerRxQ;
use super::{defs, Error, Result, VsockGenericMuxer};

/// A unique identifier of a `VsockConnection` object. Connections are stored in
/// a hash map, keyed by a `ConnMapKey` object.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ConnMapKey {
    local_port: u32,
    pub(crate) peer_port: u32,
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

/// An epoll listener, registered under the muxer's nested epoll FD.
pub enum EpollListener {
    /// The listener is a `VsockConnection`, identified by `key`, and interested
    /// in the events in `evset`. Since `VsockConnection` implements
    /// `VsockEpollListener`, notifications will be forwarded to the listener
    /// via `VsockEpollListener::notify()`.
    Connection {
        key: ConnMapKey,
        evset: epoll::Events,
        backend: VsockBackendType,
    },
    /// A listener interested in new host-initiated connections.
    Backend(VsockBackendType),
    /// A listener interested in reading host "connect <port>" commands from a
    /// freshly connected host socket.
    LocalStream(Box<dyn VsockStream>),
}

impl PartialEq for EpollListener {
    fn eq(&self, other: &EpollListener) -> bool {
        match (self, other) {
            (
                EpollListener::Connection {
                    key: key1,
                    evset: evt1,
                    backend: be1,
                },
                EpollListener::Connection {
                    key: key2,
                    evset: evt2,
                    backend: be2,
                },
            ) => key1 == key2 && evt1 == evt2 && be1 == be2,
            (EpollListener::Backend(type1), EpollListener::Backend(type2)) => type1 == type2,
            (EpollListener::LocalStream(stream1), EpollListener::LocalStream(stream2)) => {
                stream1.as_raw_fd() == stream2.as_raw_fd()
                    && stream1.backend_type() == stream2.backend_type()
            }
            _ => false,
        }
    }
}

/// The vsock connection multiplexer.
pub struct VsockMuxer {
    /// Guest CID.
    cid: u64,
    /// A hash map used to store the active connections.
    conn_map: HashMap<ConnMapKey, VsockConnection>,
    /// A hash map used to store epoll event listeners / handlers.
    listener_map: HashMap<RawFd, EpollListener>,
    /// The RX queue. Items in this queue are consumed by
    /// `VsockMuxer::recv_pkt()`, and produced
    /// - by `VsockMuxer::send_pkt()` (e.g. RST in response to a connection
    ///   request packet); and
    /// - in response to EPOLLIN events (e.g. data available to be read from an
    ///   AF_UNIX socket).
    rxq: MuxerRxQ,
    /// A queue used for terminating connections that are taking too long to
    /// shut down.
    killq: MuxerKillQ,
    /// The nested epoll FD, used to register epoll listeners.
    epoll_fd: RawFd,
    /// A hash set used to keep track of used host-side (local) ports, in order
    /// to assign local ports to host-initiated connections.
    local_port_set: HashSet<u32>,
    /// The last used host-side port.
    local_port_last: u32,
    /// backend implementations supported in muxer.
    backend_map: HashMap<VsockBackendType, Box<dyn VsockBackend>>,
    /// the backend which can accept peer-initiated connection.
    peer_backend: Option<VsockBackendType>,
}

impl VsockChannel for VsockMuxer {
    /// Deliver a vsock packet to the guest vsock driver.
    ///
    /// Retuns:
    /// - `Ok(())`: `pkt` has been successfully filled in; or
    /// - `Err(VsockError::NoData)`: there was no available data with which to fill in the packet.
    fn recv_pkt(&mut self, pkt: &mut VsockPacket) -> VsockResult<()> {
        // We'll look for instructions on how to build the RX packet in the RX
        // queue. If the queue is empty, that doesn't necessarily mean we don't
        // have any pending RX, since the queue might be out-of-sync. If that's
        // the case, we'll attempt to sync it first, and then try to pop
        // something out again.
        if self.rxq.is_empty() && !self.rxq.is_synced() {
            self.rxq = MuxerRxQ::from_conn_map(&self.conn_map);
        }

        while let Some(rx) = self.rxq.peek() {
            let res = match rx {
                // We need to build an RST packet, going from `local_port` to
                // `peer_port`.
                MuxerRx::RstPkt {
                    local_port,
                    peer_port,
                } => {
                    pkt.set_op(uapi::VSOCK_OP_RST)
                        .set_src_cid(uapi::VSOCK_HOST_CID)
                        .set_dst_cid(self.cid)
                        .set_src_port(local_port)
                        .set_dst_port(peer_port)
                        .set_len(0)
                        .set_type(uapi::VSOCK_TYPE_STREAM)
                        .set_flags(0)
                        .set_buf_alloc(0)
                        .set_fwd_cnt(0);
                    self.rxq.pop().unwrap();
                    trace!(
                        "vsock: muxer.recv[rxq.len={}, type={}, op={}, sp={}, sc={}, dp={}, dc={}]: {:?}",
                        self.rxq.len(),
                        pkt.type_(),
                        pkt.op(),
                        pkt.src_port(),
                        pkt.src_cid(),
                        pkt.dst_port(),
                        pkt.dst_cid(),
                        pkt.hdr()
                    );
                    return Ok(());
                }

                // We'll defer building the packet to this connection, that has
                // something to say.
                MuxerRx::ConnRx(key) => {
                    let mut conn_res = Err(VsockError::NoData);
                    let mut do_pop = true;
                    self.apply_conn_mutation(key, |conn| {
                        conn_res = conn.recv_pkt(pkt);
                        do_pop = !conn.has_pending_rx();
                    });
                    if do_pop {
                        self.rxq.pop().unwrap();
                    }
                    conn_res
                }
            };

            if res.is_ok() {
                // Inspect traffic, looking for RST packets, since that means we
                // have to terminate and remove this connection from the active
                // connection pool.
                if pkt.op() == uapi::VSOCK_OP_RST {
                    self.remove_connection(ConnMapKey {
                        local_port: pkt.src_port(),
                        peer_port: pkt.dst_port(),
                    });
                }

                trace!(
                    "vsock: muxer.recv[rxq.len={}, type={}, op={}, sp={}, sc={}, dp={}, dc={}]: {:?}",
                    self.rxq.len(),
                    pkt.type_(),
                    pkt.op(),
                    pkt.src_port(),
                    pkt.src_cid(),
                    pkt.dst_port(),
                    pkt.dst_cid(),
                    pkt.hdr()
                );
                return Ok(());
            }
        }

        Err(VsockError::NoData)
    }

    /// Deliver a guest-generated packet to its destination in the vsock
    /// backend.
    ///
    /// This absorbs unexpected packets, handles RSTs (by dropping connections),
    /// and forwards all the rest to their owning `VsockConnection`.
    ///
    /// Returns: always `Ok(())` - the packet has been consumed, and its virtio
    /// TX buffers can be returned to the guest vsock driver.
    fn send_pkt(&mut self, pkt: &VsockPacket) -> VsockResult<()> {
        let conn_key = ConnMapKey {
            local_port: pkt.dst_port(),
            peer_port: pkt.src_port(),
        };

        trace!(
            "vsock: muxer.send[rxq.len={}, type={}, op={}, sp={}, sc={}, dp={}, dc={}]: {:?}",
            self.rxq.len(),
            pkt.type_(),
            pkt.op(),
            pkt.src_port(),
            pkt.src_cid(),
            pkt.dst_port(),
            pkt.dst_cid(),
            pkt.hdr()
        );

        // If this packet has an unsupported type (!=stream), we must send back
        // an RST.
        if pkt.type_() != uapi::VSOCK_TYPE_STREAM {
            self.enq_rst(pkt.dst_port(), pkt.src_port());
            return Ok(());
        }

        // We don't know how to handle packets addressed to other CIDs. We only
        // handle the host part of the guest - host communication here.
        if pkt.dst_cid() != uapi::VSOCK_HOST_CID {
            info!(
                "vsock: dropping guest packet for unknown CID: {:?}",
                pkt.hdr()
            );
            return Ok(());
        }

        if !self.conn_map.contains_key(&conn_key) {
            // This packet can't be routed to any active connection (based on
            // its src and dst ports). The only orphan / unroutable packets we
            // know how to handle are connection requests.
            if pkt.op() == uapi::VSOCK_OP_REQUEST {
                // Oh, this is a connection request!
                self.handle_peer_request_pkt(&pkt);
            } else {
                // Send back an RST, to let the drive know we weren't expecting
                // this packet.
                self.enq_rst(pkt.dst_port(), pkt.src_port());
            }
            return Ok(());
        }

        // Right, we know where to send this packet, then (to `conn_key`).
        // However, if this is an RST, we have to forcefully terminate the
        // connection, so there's no point in forwarding it the packet.
        if pkt.op() == uapi::VSOCK_OP_RST {
            self.remove_connection(conn_key);
            return Ok(());
        }

        // Alright, everything looks in order - forward this packet to its
        // owning connection.
        let mut res: VsockResult<()> = Ok(());
        self.apply_conn_mutation(conn_key, |conn| {
            res = conn.send_pkt(pkt);
        });

        res
    }

    /// Check if the muxer has any pending RX data, with which to fill a
    /// guest-provided RX buffer.
    fn has_pending_rx(&self) -> bool {
        !self.rxq.is_empty() || !self.rxq.is_synced()
    }
}

impl AsRawFd for VsockMuxer {
    /// Get the FD to be registered for polling upstream (in the main VMM epoll
    /// loop, in this case).
    ///
    /// This will be the muxer's nested epoll FD.
    fn as_raw_fd(&self) -> RawFd {
        self.epoll_fd
    }
}

impl VsockEpollListener for VsockMuxer {
    /// Get the epoll events to be polled upstream.
    ///
    /// Since the polled FD is a nested epoll FD, we're only interested in
    /// EPOLLIN events (i.e. some event occured on one of the FDs registered
    /// under our epoll FD).
    fn get_polled_evset(&self) -> epoll::Events {
        epoll::Events::EPOLLIN
    }

    /// Notify the muxer about a pending event having occured under its nested
    /// epoll FD.
    fn notify(&mut self, _: epoll::Events) {
        trace!("vsock: muxer received kick");

        let mut epoll_events = vec![epoll::Event::new(epoll::Events::empty(), 0); 32];
        match epoll::wait(self.epoll_fd, 0, epoll_events.as_mut_slice()) {
            Ok(ev_cnt) => {
                for ev in &epoll_events[0..ev_cnt] {
                    self.handle_event(
                        ev.data as RawFd,
                        epoll::Events::from_bits(ev.events).unwrap(),
                    );
                }
            }
            Err(e) => {
                warn!("vsock: failed to consume muxer epoll event: {}", e);
            }
        }
    }
}

impl VsockGenericMuxer for VsockMuxer {
    /// add a backend for Muxer.
    fn add_backend(&mut self, backend: Box<dyn VsockBackend>, is_peer_backend: bool) -> Result<()> {
        let backend_type = backend.r#type();
        if self.backend_map.contains_key(&backend_type) {
            return Err(Error::BackendRegistered(backend_type));
        }
        self.add_listener(
            backend.as_raw_fd(),
            EpollListener::Backend(backend_type.clone()),
        )?;
        self.backend_map.insert(backend_type.clone(), backend);
        if is_peer_backend {
            self.peer_backend = Some(backend_type);
        }
        Ok(())
    }
}

impl VsockMuxer {
    /// Muxer constructor.
    pub fn new(cid: u64) -> Result<Self> {
        Ok(Self {
            cid,
            epoll_fd: epoll::create(false).map_err(Error::EpollFdCreate)?,
            rxq: MuxerRxQ::default(),
            conn_map: HashMap::with_capacity(defs::MAX_CONNECTIONS),
            listener_map: HashMap::with_capacity(defs::MAX_CONNECTIONS + 1),
            killq: MuxerKillQ::default(),
            local_port_last: (1u32 << 30) - 1,
            local_port_set: HashSet::with_capacity(defs::MAX_CONNECTIONS),
            backend_map: HashMap::new(),
            peer_backend: None,
        })
    }

    /// Handle/dispatch an epoll event to its listener.
    fn handle_event(&mut self, fd: RawFd, evset: epoll::Events) {
        trace!(
            "vsock: muxer processing event: fd={}, evset={:?}",
            fd,
            evset
        );

        match self.listener_map.get_mut(&fd) {
            // This event needs to be forwarded to a `VsockConnection` that is
            // listening for it.
            Some(EpollListener::Connection { key, evset, .. }) => {
                let key_copy = *key;
                let evset_copy = *evset;
                // The handling of this event will most probably mutate the
                // state of the receiving conection. We'll need to check for new
                // pending RX, event set mutation, and all that, so we're
                // wrapping the event delivery inside those checks.
                self.apply_conn_mutation(key_copy, |conn| {
                    conn.notify(evset_copy);
                });
            }

            // A new host-initiated connection is ready to be accepted.
            Some(EpollListener::Backend(backend_type)) => {
                if let Some(backend) = self.backend_map.get_mut(backend_type) {
                    if self.rxq.len() == defs::MAX_CONNECTIONS {
                        // If we're already maxed-out on connections, we'll just
                        // accept and immediately discard this potentially new
                        // one.
                        warn!("vsock: connection limit reached; refusing new host connection");
                        backend.accept().map(|_| 0).unwrap_or(0);
                        return;
                    }
                    backend
                        .accept()
                        .map_err(Error::BackendAccept)
                        .and_then(|stream| {
                            // Before forwarding this connection to a listening
                            // AF_VSOCK socket on the guest side, we need to
                            // know the destination port. We'll read that port
                            // from a "connect" command received on this socket,
                            // so the next step is to ask to be notified the
                            // moment we can read from it.
                            self.add_listener(
                                stream.as_raw_fd(),
                                EpollListener::LocalStream(stream),
                            )
                        })
                        .unwrap_or_else(|err| {
                            warn!("vsock: unable to accept local connection: {:?}", err);
                        });
                } else {
                    error!("vsock: unsable to find specific backend {:?}", backend_type)
                }
            }

            // Data is ready to be read from a host-initiated connection. That
            // would be the "connect" command that we're expecting.
            Some(EpollListener::LocalStream(_)) => {
                if let Some(EpollListener::LocalStream(mut stream)) = self.remove_listener(fd) {
                    Self::read_local_stream_port(&mut stream)
                        .map(|peer_port| (self.allocate_local_port(), peer_port))
                        .and_then(|(local_port, peer_port)| {
                            self.add_connection(
                                ConnMapKey {
                                    local_port,
                                    peer_port,
                                },
                                VsockConnection::new_local_init(
                                    stream,
                                    uapi::VSOCK_HOST_CID,
                                    self.cid,
                                    local_port,
                                    peer_port,
                                ),
                            )
                        })
                        .unwrap_or_else(|err| {
                            info!("vsock: error adding local-init connection: {:?}", err);
                        })
                }
            }

            _ => {
                info!("vsock: unexpected event: fd={:?}, evset={:?}", fd, evset);
            }
        }
    }

    /// Parse a host "connect" command, and extract the destination vsock port.
    fn read_local_stream_port(stream: &mut Box<dyn VsockStream>) -> Result<u32> {
        let mut buf = [0u8; 32];

        // This is the minimum number of bytes that we should be able to read,
        // when parsing a valid connection request. I.e. `b"connect 0\n".len()`.
        const MIN_READ_LEN: usize = 10;

        // Bring in the minimum number of bytes that we should be able to read.
        stream
            .read(&mut buf[..MIN_READ_LEN])
            .map_err(Error::BackendRead)?;

        // Now, finish reading the destination port number, by bringing in one
        // byte at a time, until we reach an EOL terminator (or our buffer space
        // runs out).  Yeah, not particularly proud of this approach, but it
        // will have to do for now.
        let mut blen = MIN_READ_LEN;
        while buf[blen - 1] != b'\n' && blen < buf.len() {
            stream
                .read_exact(&mut buf[blen..=blen])
                .map_err(Error::BackendRead)?;
            blen += 1;
        }

        let mut word_iter = std::str::from_utf8(&buf)
            .map_err(|_| Error::InvalidPortRequest)?
            .split_whitespace();

        word_iter
            .next()
            .ok_or(Error::InvalidPortRequest)
            .and_then(|word| {
                if word.to_lowercase() == "connect" {
                    Ok(())
                } else {
                    Err(Error::InvalidPortRequest)
                }
            })
            .and_then(|_| word_iter.next().ok_or(Error::InvalidPortRequest))
            .and_then(|word| word.parse::<u32>().map_err(|_| Error::InvalidPortRequest))
            .map_err(|_| Error::InvalidPortRequest)
    }

    /// Add a new connection to the active connection pool.
    fn add_connection(&mut self, key: ConnMapKey, conn: VsockConnection) -> Result<()> {
        // We might need to make room for this new connection, so let's sweep
        // the kill queue first.  It's fine to do this here because:
        // - unless the kill queue is out of sync, this is a pretty inexpensive
        //   operation; and
        // - we are under no pressure to respect any accurate timing for
        //   connection termination.
        self.sweep_killq();

        if self.conn_map.len() >= defs::MAX_CONNECTIONS {
            info!(
                "vsock: muxer connection limit reached ({})",
                defs::MAX_CONNECTIONS
            );
            return Err(Error::TooManyConnections);
        }

        self.add_listener(
            conn.as_raw_fd(),
            EpollListener::Connection {
                key,
                evset: conn.get_polled_evset(),
                backend: conn.stream.backend_type(),
            },
        )
        .map(|_| {
            if conn.has_pending_rx() {
                // We can safely ignore any error in adding a connection RX
                // indication. Worst case scenario, the RX queue will get
                // desynchronized, but we'll handle that the next time we need
                // to yield an RX packet.
                self.rxq.push(MuxerRx::ConnRx(key));
            }
            self.conn_map.insert(key, conn);
        })
    }

    /// Remove a connection from the active connection poll.
    fn remove_connection(&mut self, key: ConnMapKey) {
        if let Some(conn) = self.conn_map.remove(&key) {
            self.remove_listener(conn.as_raw_fd());
        }
        self.free_local_port(key.local_port);
    }

    /// Schedule a connection for immediate termination. I.e. as soon as we can
    /// also let our peer know we're dropping the connection, by sending it an
    /// RST packet.
    fn kill_connection(&mut self, key: ConnMapKey) {
        let mut had_rx = false;

        self.conn_map.entry(key).and_modify(|conn| {
            had_rx = conn.has_pending_rx();
            conn.kill();
        });
        // This connection will now have an RST packet to yield, so we need to
        // add it to the RX queue. However, there's no point in doing that if it
        // was already in the queue.
        if !had_rx {
            // We can safely ignore any error in adding a connection RX
            // indication. Worst case scenario, the RX queue will get
            // desynchronized, but we'll handle that the next time we need to
            // yield an RX packet.
            self.rxq.push(MuxerRx::ConnRx(key));
        }
    }

    /// Register a new epoll listener under the muxer's nested epoll FD.
    pub(crate) fn add_listener(&mut self, fd: RawFd, listener: EpollListener) -> Result<()> {
        let evset = match listener {
            EpollListener::Connection { evset, .. } => evset,
            EpollListener::LocalStream(_) => epoll::Events::EPOLLIN,
            EpollListener::Backend(_) => epoll::Events::EPOLLIN,
        };

        epoll::ctl(
            self.epoll_fd,
            epoll::ControlOptions::EPOLL_CTL_ADD,
            fd,
            epoll::Event::new(evset, fd as u64),
        )
        .map(|_| {
            self.listener_map.insert(fd, listener);
        })
        .map_err(Error::EpollAdd)?;

        Ok(())
    }

    /// Remove (and return) a previously registered epoll listener.
    fn remove_listener(&mut self, fd: RawFd) -> Option<EpollListener> {
        let maybe_listener = self.listener_map.remove(&fd);

        if maybe_listener.is_some() {
            epoll::ctl(
                self.epoll_fd,
                epoll::ControlOptions::EPOLL_CTL_DEL,
                fd,
                epoll::Event::new(epoll::Events::empty(), 0),
            )
            .unwrap_or_else(|err| {
                warn!(
                    "vosck muxer: error removing epoll listener for fd {:?}: {:?}",
                    fd, err
                );
            });
        }

        maybe_listener
    }

    /// Allocate a host-side port to be assigned to a new host-initiated
    /// connection.
    fn allocate_local_port(&mut self) -> u32 {
        // TODO: this doesn't seem very space-efficient.
        // Mybe rewrite this to limit port range and use a bitmap?

        loop {
            self.local_port_last = (self.local_port_last + 1) & !(1 << 31) | (1 << 30);
            if self.local_port_set.insert(self.local_port_last) {
                break;
            }
        }
        self.local_port_last
    }

    /// Mark a previously used host-side port as free.
    fn free_local_port(&mut self, port: u32) {
        self.local_port_set.remove(&port);
    }

    /// Handle a new connection request comming from our peer (the guest vsock
    /// driver).
    ///
    /// This will attempt to connect to a host-side backend. If successful, a
    ///  new connection object will be created and added to the connection pool.
    ///  On failure, a new RST packet will be scheduled for delivery to the
    ///  guest.
    fn handle_peer_request_pkt(&mut self, pkt: &VsockPacket) {
        if self.peer_backend.is_none() {
            error!("no usable backend for peer request");
            self.enq_rst(pkt.dst_port(), pkt.src_port());
            return;
        }

        // safe to unwrap
        if let Some(backend) = self.backend_map.get(self.peer_backend.as_ref().unwrap()) {
            backend
                .connect(pkt.dst_port())
                .map_err(Error::BackendConnect)
                .and_then(|stream| {
                    self.add_connection(
                        ConnMapKey {
                            local_port: pkt.dst_port(),
                            peer_port: pkt.src_port(),
                        },
                        VsockConnection::new_peer_init(
                            stream,
                            uapi::VSOCK_HOST_CID,
                            self.cid,
                            pkt.dst_port(),
                            pkt.src_port(),
                            pkt.buf_alloc(),
                        ),
                    )
                })
                .unwrap_or_else(|e| {
                    error!("peer request error: {:?}", e);
                    self.enq_rst(pkt.dst_port(), pkt.src_port());
                });
        } else {
            error!("no usable backend selected for peer request");
            self.enq_rst(pkt.dst_port(), pkt.src_port());
        }
    }

    /// Perform an action that might mutate a connection's state.
    ///
    /// This is used as shorthand for repetitive tasks that need to be performed
    /// after a connection object mutates. E.g.
    /// - update the connection's epoll listener;
    /// - schedule the connection to be queried for RX data;
    /// - kill the connection if an unrecoverable error occurs.
    fn apply_conn_mutation<F>(&mut self, key: ConnMapKey, mut_fn: F)
    where
        F: FnOnce(&mut VsockConnection),
    {
        if let Some(conn) = self.conn_map.get_mut(&key) {
            let had_rx = conn.has_pending_rx();
            let was_expiring = conn.will_expire();
            let prev_state = conn.state();
            let backend_type = conn.stream.backend_type();

            mut_fn(conn);

            // If this is a host-initiated connection that has just become
            // established, we'll have to send an ack message to the host end.
            if prev_state == ConnState::LocalInit && conn.state() == ConnState::Established {
                let msg = format!("OK {}\n", key.local_port);
                match conn.send_bytes_raw(msg.as_bytes()) {
                    Ok(written) if written == msg.len() => (),
                    Ok(_) => {
                        // If we can't write a dozen bytes to a pristine
                        // connection something must be really wrong. Killing
                        // it.
                        conn.kill();
                        warn!("vsock: unable to fully write connection ack msg.");
                    }
                    Err(err) => {
                        conn.kill();
                        warn!("vsock: unable to ack host connection [local_cid {}, peer_cid {}, local_port {}, peer_port {}]: {:?}", conn.local_cid, conn.peer_cid, conn.local_port, conn.peer_port, err);
                    }
                };
            }

            // If the connection wasn't previously scheduled for RX, add it to
            // our RX queue.
            if !had_rx && conn.has_pending_rx() {
                self.rxq.push(MuxerRx::ConnRx(key));
            }

            // If the connection wasn't previously scheduled for termination,
            // add it to the kill queue.
            if !was_expiring && conn.will_expire() {
                // It's safe to unwrap here, since `conn.will_expire()` already
                // guaranteed that an `conn.expiry` is available.
                self.killq.push(key, conn.expiry().unwrap());
            }

            let fd = conn.as_raw_fd();
            let new_evset = conn.get_polled_evset();
            if new_evset.is_empty() {
                // If the connection no longer needs epoll notifications, remove
                // its listener from our list.
                self.remove_listener(fd);
                return;
            }
            if let Some(EpollListener::Connection { evset, .. }) = self.listener_map.get_mut(&fd) {
                if *evset != new_evset {
                    // If the set of events that the connection is interested in
                    // has changed, we need to update its epoll listener.
                    debug!(
                        "vsock: updating listener for (lp={}, pp={}): old={:?}, new={:?}",
                        key.local_port, key.peer_port, *evset, new_evset
                    );

                    *evset = new_evset;
                    epoll::ctl(
                        self.epoll_fd,
                        epoll::ControlOptions::EPOLL_CTL_MOD,
                        fd,
                        epoll::Event::new(new_evset, fd as u64),
                    )
                    .unwrap_or_else(|err| {
                        // This really shouldn't happen, like, ever. However,
                        // "famous last words" and all that, so let's just kill
                        // it with fire, and walk away.
                        self.kill_connection(key);
                        warn!(
                            "vsock: error updating epoll listener for (lp={}, pp={}): {:?}",
                            key.local_port, key.peer_port, err
                        );
                    });
                }
            } else {
                // The connection had previously asked to be removed from the
                // listener map (by returning an empty event set via
                // `get_polled_fd()`), but now wants back in.
                self.add_listener(
                    fd,
                    EpollListener::Connection {
                        key,
                        evset: new_evset,
                        backend: backend_type,
                    },
                )
                .unwrap_or_else(|err| {
                    self.kill_connection(key);
                    warn!(
                        "vsock: error updating epoll listener for (lp={}, pp={}): {:?}",
                        key.local_port, key.peer_port, err
                    );
                });
            }
        }
    }

    /// Check if any connections have timed out, and if so, schedule them for
    /// immediate termination.
    fn sweep_killq(&mut self) {
        while let Some(key) = self.killq.pop() {
            // Connections don't get removed from the kill queue when their kill
            // timer is disarmed, since that would be a costly operation. This
            // means we must check if the connection has indeed expired, prior
            // to killing it.
            let mut kill = false;
            self.conn_map
                .entry(key)
                .and_modify(|conn| kill = conn.has_expired());
            if kill {
                self.kill_connection(key);
            }
        }

        if self.killq.is_empty() && !self.killq.is_synced() {
            self.killq = MuxerKillQ::from_conn_map(&self.conn_map);
            // If we've just re-created the kill queue, we can sweep it again;
            // maybe there's more to kill.
            self.sweep_killq();
        }
    }

    /// Enqueue an RST packet into `self.rxq`.
    ///
    /// Enqueue errors aren't propagated up the call chain, since there is
    /// nothing we can do to handle them. We do, however, log a warning, since
    /// not being able to enqueue an RST packet means we have to drop it, which
    /// is not normal operation.
    fn enq_rst(&mut self, local_port: u32, peer_port: u32) {
        let pushed = self.rxq.push(MuxerRx::RstPkt {
            local_port,
            peer_port,
        });
        if !pushed {
            warn!(
                "vsock: muxer.rxq full; dropping RST packet for lp={}, pp={}",
                local_port, peer_port
            );
        }
    }
}
