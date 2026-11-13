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

#![deny(unreachable_pub)]
#![deny(warnings)]
use clap::Parser;

use base_crypto::data_provider::{FetchMode, MidnightDataProvider, OutputMode};
use futures::future::join;
use ledger::dust::DustResolver;
use ledger::prove::Resolver;
use midnight_proof_server::endpoints::PUBLIC_PARAMS;
use midnight_proof_server::{server, worker_pool::WorkerPool};
use tracing::{Level, info, warn};
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Layer, Registry};
use transient_crypto::proofs::{KeyLocation, Resolver as ResolverT};
use zkir::{GpuCalibration, GpuLineStatus};

#[derive(Parser, Debug)]
#[cfg_attr(feature = "gpu", command(after_help = AUTOTUNE_HELP))]
struct Args {
    #[arg(
        short,
        long,
        default_value_t = 6300,
        env = "MIDNIGHT_PROOF_SERVER_PORT"
    )]
    port: u16,
    #[arg(short, long, env = "MIDNIGHT_PROOF_SERVER_VERBOSE")]
    verbose: bool,
    #[arg(long, default_value_t = 10, env = "MIDNIGHT_PROOF_SERVER_JOB_CAPACITY")]
    job_capacity: usize,
    #[arg(long, default_value_t = 2, env = "MIDNIGHT_PROOF_SERVER_NUM_WORKERS")]
    num_workers: usize,
    #[arg(
        long,
        default_value_t = 600.0,
        env = "MIDNIGHT_PROOF_SERVER_JOB_TIMEOUT"
    )]
    job_timeout: f64,
    #[arg(
        long,
        default_value_t = false,
        env = "MIDNIGHT_PROOF_SERVER_NO_FETCH_PARAMS"
    )]
    no_fetch_params: bool,
    /// Prover parameters kept in memory between proofs, in MiB of base points: the
    /// most recently used sizes that fit (0 reads them again for every proof).
    #[cfg(feature = "gpu")]
    #[arg(
        long,
        default_value_t = midnight_proof_server::params_cache::DEFAULT_PARAMS_CACHE_MIB,
        env = "MIDNIGHT_PROOF_SERVER_PARAMS_CACHE_MIB"
    )]
    params_cache_mib: u64,
}

/// The `autotune` subcommand, listed under `--help` in GPU builds.
#[cfg(feature = "gpu")]
const AUTOTUNE_HELP: &str = "\
GPU calibration:
  midnight-proof-server autotune [--refresh | --show] [--quiet]
      Measures this machine's GPU/CPU crossovers and persists them, unless
      they are persisted already (--refresh re-measures; --show prints the
      values in effect and never measures). The server loads them at start-up,
      measuring first if there are none. The file is MIDNIGHT_AUTOTUNE_CACHE,
      else midnight-gpu/autotune-v1.conf in $XDG_CACHE_HOME or ~/.cache.";

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // GPU builds mount the CUDA backend's calibration command as a subcommand.
    // It is dispatched before clap parses, so its flags, output and exit codes
    // are those of the backend's standalone `midnight-gpu-autotune`.
    #[cfg(feature = "gpu")]
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "autotune")
    {
        let code = zkir::autotune_cli("midnight-proof-server autotune", std::env::args().skip(2));
        std::process::exit(code.into());
    }
    let args = Args::parse();
    init_logging(args.verbose);
    // GPU builds (`--features gpu`): load and warm the CUDA backend once, on the
    // main thread, before the worker pool exists, and make its calibration
    // effective (measuring it first if none is persisted for this machine), so
    // the first proof pays for neither and a misconfigured GPU deployment is
    // visible at startup.
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
    #[cfg(feature = "gpu")]
    {
        midnight_proof_server::params_cache::set_budget_mib(args.params_cache_mib);
        info!(
            "GPU proving: prover parameters kept in memory up to {} MiB",
            args.params_cache_mib
        );
    }
    if !args.no_fetch_params {
        info!("Ensuring zswap key material is available...");
        let resolver = Resolver::new(
            PUBLIC_PARAMS.clone(),
            DustResolver(
                MidnightDataProvider::new(
                    FetchMode::OnDemand,
                    OutputMode::Log,
                    ledger::dust::DUST_EXPECTED_FILES.to_owned(),
                )
                .expect("data provider initialization failed"),
            ),
            Box::new(move |_: KeyLocation| Box::pin(std::future::ready(Ok(None)))),
        );
        let ks = futures::future::join_all((10..=15).map(|k| PUBLIC_PARAMS.0.fetch_k(k)));
        let keys = futures::future::join_all(
            [
                "midnight/zswap/spend",
                "midnight/zswap/output",
                "midnight/zswap/sign",
                "midnight/dust/spend",
            ]
            .into_iter()
            .map(|k| resolver.resolve_key(KeyLocation(k.into()))),
        );
        let (ks, keys) = join(ks, keys).await;
        ks.into_iter().collect::<Result<Vec<_>, _>>()?;
        keys.into_iter().collect::<Result<Vec<_>, _>>()?;
    }
    let pool = WorkerPool::new(args.num_workers, args.job_capacity, args.job_timeout);
    server(args.port, !args.no_fetch_params, pool)
        .unwrap()
        .0
        .await
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

fn init_logging(verbose: bool) {
    let level = if verbose { Level::DEBUG } else { Level::INFO };
    Registry::default()
        .with(
            tracing_subscriber::fmt::layer().with_filter(
                Targets::new()
                    .with_default(level)
                    .with_target("zkir", tracing_subscriber::filter::LevelFilter::OFF),
            ),
        )
        .try_init()
        .ok();
}
