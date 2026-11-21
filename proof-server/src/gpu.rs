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

//! GPU proving at start-up: the `autotune` subcommand and the parameter cache's
//! option (GPU builds), and the backend's initialization with its log lines
//! (every build; without GPU support no line is compiled in and nothing is
//! logged).

use tracing::{info, warn};
use zkir::{GpuCalibration, GpuLineStatus};

// The options of GPU builds. (A plain comment: clap would turn a doc comment on a
// flattened group into the command's description in `--help`.)
#[cfg(feature = "gpu")]
#[derive(clap::Args, Debug)]
pub(crate) struct GpuArgs {
    /// Prover parameters kept in memory between proofs, in MiB of base points: the
    /// most recently used sizes that fit (0 reads them again for every proof).
    #[arg(
        long,
        default_value_t = midnight_proof_server::params_cache::DEFAULT_PARAMS_CACHE_MIB,
        env = "MIDNIGHT_PROOF_SERVER_PARAMS_CACHE_MIB"
    )]
    params_cache_mib: u64,
}

/// The `autotune` subcommand, listed under `--help` in GPU builds.
#[cfg(feature = "gpu")]
pub(crate) const AUTOTUNE_HELP: &str = "\
GPU calibration:
  midnight-proof-server autotune [--refresh | --show] [--quiet]
      Measures this machine's GPU/CPU crossovers and persists them, unless
      they are persisted already (--refresh re-measures; --show prints the
      values in effect and never measures). The server loads them at start-up,
      measuring first if there are none. The file is MIDNIGHT_AUTOTUNE_CACHE,
      else midnight-gpu/autotune-v1.conf in $XDG_CACHE_HOME or ~/.cache.";

/// Runs `midnight-proof-server autotune ...` and exits with its code, if that
/// is the command line. GPU builds mount the CUDA backend's calibration command
/// as a subcommand. It is dispatched before clap parses, so its flags, output
/// and exit codes are those of the backend's standalone `midnight-gpu-autotune`.
#[cfg(feature = "gpu")]
pub(crate) fn run_autotune_if_asked() {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "autotune")
    {
        let code = zkir::autotune_cli("midnight-proof-server autotune", std::env::args().skip(2));
        std::process::exit(code.into());
    }
}

/// GPU builds (`--features gpu`): loads and warms the CUDA backend once, on the
/// main thread, before the worker pool exists, and makes its calibration
/// effective (measuring it first if none is persisted for this machine), so the
/// first proof pays for neither and a misconfigured GPU deployment is visible at
/// startup.
pub(crate) fn init_backend() {
    let gpu = zkir::init_gpu_backend();
    for (line, status, calibration) in gpu.lines_with_calibration() {
        match status {
            GpuLineStatus::Initialized(warmup) => {
                info!("GPU proving backend for {line} initialized (warm-up {warmup:?})");
                if let Some(calibration) = calibration {
                    log_gpu_calibration(line, calibration);
                }
            }
            GpuLineStatus::Failed => warn!(
                "GPU proving backend for {line} failed to initialize (cause on stderr); \
                 its proofs fall back to CPU wherever the GPU is unusable"
            ),
            GpuLineStatus::Skipped => {
                info!("MIDNIGHT_DEVICE=cpu: GPU proving backend for {line} not initialized")
            }
            GpuLineStatus::NotCompiled => {}
        }
    }
    // Both lines share one backend instance and so one calibration; what is left
    // to report is the device mode and the thresholds it routes work by.
    if let Some(info) = zkir::gpu_backend_info() {
        info!("GPU proving: {info}");
    }
}

/// Sets the budget of the prover parameters kept in memory (`--params-cache-mib`).
#[cfg(feature = "gpu")]
pub(crate) fn set_params_cache_budget(args: &GpuArgs) {
    midnight_proof_server::params_cache::set_budget_mib(args.params_cache_mib);
    info!(
        "GPU proving: prover parameters kept in memory up to {} MiB",
        args.params_cache_mib
    );
}

/// Logs where a line's GPU/CPU crossovers come from, warning if any is the
/// backend's built-in default rather than measured on this machine.
fn log_gpu_calibration(line: &str, calibration: GpuCalibration) {
    info!("GPU calibration for {line}: {calibration}");
    if calibration.on_fallback() {
        match calibration.source() {
            zkir::GpuCalibrationSource::Disabled => warn!(
                "MIDNIGHT_AUTOTUNE=0: {line} proves with the built-in GPU crossovers, \
                 not ones measured on this machine"
            ),
            _ => warn!(
                "{line} proves with the built-in GPU crossovers: no calibration could be \
                 measured or loaded; check `midnight-proof-server autotune --show`"
            ),
        }
    }
}
