// Copyright (C) 2021 Alibaba Cloud. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

/// This module implements backends for vsock - the host side vsock endpoint,
/// which can translate vsock stream into host's protocol, eg. AF_UNIX, AF_INET
/// or even the protocol created by us.
use std::fmt;
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::time::Duration;

use downcast_rs::{impl_downcast, Downcast};

mod inner;

pub use self::inner::{VsockInnerBackend, VsockInnerConnector, VsockInnerStream};

/// The type of vsock backend.
#[derive(PartialEq, Eq, Hash, Debug, Clone)]
pub enum VsockBackendType {
    /// Unix Domain Socket
    Uds,
    /// Tcp socket
    Tcp,
    /// Inner backend
    Inner,
    /// For Test purpose
    #[cfg(test)]
    Test,
}

impl fmt::Display for VsockBackendType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self)
    }
}

/// The generic abstract of Vsock Backend, looks like socket's API.
pub trait VsockBackend: AsRawFd + Send + Downcast {
    /// Accept a host-initiated connection.
    fn accept(&mut self) -> std::io::Result<Box<dyn VsockStream>>;
    /// Connect by a guest-initiated connection.
    fn connect(&self, dst_port: u32) -> std::io::Result<Box<dyn VsockStream>>;
    /// The type of backend.
    fn r#type(&self) -> VsockBackendType;
}
impl_downcast!(VsockBackend);

/// The generic abstract of Vsock Stream.
pub trait VsockStream: Read + Write + AsRawFd + Send + Downcast {
    /// The type of backend which created the stream.
    fn backend_type(&self) -> VsockBackendType;
    /// Moves VsockStream into or out of nonblocking mode
    fn set_nonblocking(&mut self, _nonblocking: bool) -> std::io::Result<()> {
        Err(std::io::Error::from(std::io::ErrorKind::WouldBlock))
    }
    /// Set the read timeout to the time duration specified.
    fn set_read_timeout(&mut self, _dur: Option<Duration>) -> std::io::Result<()> {
        Err(std::io::Error::from(std::io::ErrorKind::InvalidInput))
    }
    /// Set the write timeout to the time duration specified.
    fn set_write_timeout(&mut self, _dur: Option<Duration>) -> std::io::Result<()> {
        Err(std::io::Error::from(std::io::ErrorKind::InvalidInput))
    }
}
impl_downcast!(VsockStream);
