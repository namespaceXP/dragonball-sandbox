// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the THIRD-PARTY file.

pub mod backend;

#[derive(Debug, thiserror::Error)]
pub enum VsockError {
    /// vsock backend error
    #[error("Vsock backend error: {0}")]
    Backend(#[source] std::io::Error),
}

type Result<T> = std::result::Result<T, VsockError>;
