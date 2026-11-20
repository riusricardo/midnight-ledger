// This file is part of midnight-ledger.
// Copyright (C) Midnight Foundation
// SPDX-License-Identifier: Apache-2.0
// Licensed under the Apache License, Version 2.0 (the "License");
// You may not use this file except in compliance with the License.
// You may obtain a copy of the License at
// http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The GPU build's command line: `autotune` reaches the CUDA backend's
//! calibration command before the server parses its own arguments, and any
//! other first argument reaches the server's parser.
#![cfg(feature = "gpu")]

use std::process::{Command, Output};

/// Runs the binary with no GPU backend and a scratch calibration cache, so
/// nothing it runs touches a device or the user's cache.
fn proof_server(args: &[&str]) -> Output {
    let scratch = std::env::temp_dir().join(format!("proof-server-gpu-cli-{}", std::process::id()));
    Command::new(env!("CARGO_BIN_EXE_midnight-proof-server"))
        .args(args)
        .env("MIDNIGHT_DEVICE", "cpu")
        .env("MIDNIGHT_AUTOTUNE_CACHE", scratch.join("autotune.conf"))
        .env("ICICLE_BACKEND_INSTALL_DIR", &scratch)
        .output()
        .expect("run the proof server binary")
}

#[test]
fn autotune_reaches_the_calibration_command() {
    let out = proof_server(&["autotune", "--help"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{out:?}");
    assert!(
        stdout.starts_with("usage: midnight-proof-server autotune [--refresh | --show] [--quiet]"),
        "{stdout}"
    );
}

#[test]
fn other_arguments_reach_the_server() {
    let out = proof_server(&["--help"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{out:?}");
    assert!(stdout.contains("--num-workers"), "{stdout}");
    assert!(stdout.contains("GPU calibration:"), "{stdout}");
}
