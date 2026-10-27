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

#[macro_use]
extern crate tracing;

use base_crypto::rng::SplittableRng;
use rand::{CryptoRng, Rng};
use transient_crypto::proofs::Proof;

mod ir;
pub mod ir_v1;
mod ir_vm;

pub use ir::{Instruction, IrMinorVersion, IrSource};
pub use ir_vm::Preprocessed;

/// Start-up state of one GPU-accelerated midnight-proofs line, as reported by
/// [`init_gpu_backend`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuLineStatus {
    /// This line's GPU support is not compiled into the build.
    NotCompiled,
    /// `MIDNIGHT_DEVICE=cpu`: the backend was not touched; the line proves on CPU.
    Skipped,
    /// The backend loaded and warmed up, taking the given time.
    Initialized(std::time::Duration),
    /// The backend did not load, or its warm-up failed; it printed the cause to
    /// stderr (`[GPU-FALLBACK]` / `[GPU-WARN]`). The line's operations fall back
    /// to CPU wherever the GPU is unusable (a panic under `MIDNIGHT_GPU_STRICT`).
    Failed,
}

/// Where a GPU/CPU crossover in effect on a line comes from. The backend's
/// autotune resolves it with the precedence its prover uses (env override,
/// then the calibration, then the built-in default), and [`autotune_cli`]'s
/// `--show` prints the same words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GpuCalibrationSource {
    /// Measured for this machine's GPU and CPU: the persisted calibration, or
    /// one this process measured.
    Calibrated,
    /// Pinned by `MIDNIGHT_GPU_MIN_K` (MSM) or `MIDNIGHT_NTT_MIN_K` (NTT).
    EnvOverride,
    /// Autotune is on but no calibration is in effect (it could not measure),
    /// so the backend's built-in default applies.
    Fallback,
    /// `MIDNIGHT_AUTOTUNE=0`: autotune is off and the built-in default applies.
    Disabled,
}

impl std::fmt::Display for GpuCalibrationSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            GpuCalibrationSource::Calibrated => "calibrated",
            GpuCalibrationSource::EnvOverride => "env-override",
            GpuCalibrationSource::Fallback => "fallback",
            GpuCalibrationSource::Disabled => "disabled",
        })
    }
}

/// The GPU/CPU crossovers one line's prover routes by, read back from the
/// backend (one instance, shared by both lines) once [`init_gpu_backend`] has
/// made its calibration effective. Under `MIDNIGHT_DEVICE=auto` (the default), MSMs of at least
/// 2^`msm_min_k` points and NTTs of at least 2^`ntt_min_k` elements run on the
/// GPU and smaller ones on CPU; `MIDNIGHT_DEVICE=gpu` sends every operation to
/// the GPU regardless.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuCalibration {
    /// log2 of the MSM crossover, in points.
    pub msm_min_k: u32,
    /// Where `msm_min_k` comes from.
    pub msm_source: GpuCalibrationSource,
    /// log2 of the NTT crossover, in elements.
    pub ntt_min_k: u32,
    /// Where `ntt_min_k` comes from.
    pub ntt_source: GpuCalibrationSource,
}

impl GpuCalibration {
    /// The line's source as one value: the crossovers' source when they agree,
    /// else the less measured of the two (in declaration order of
    /// [`GpuCalibrationSource`]).
    pub fn source(&self) -> GpuCalibrationSource {
        self.msm_source.max(self.ntt_source)
    }

    /// Whether a crossover is the backend's built-in default, neither measured
    /// on this machine nor pinned by the operator.
    pub fn on_fallback(&self) -> bool {
        self.source() >= GpuCalibrationSource::Fallback
    }
}

impl std::fmt::Display for GpuCalibration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (msm, ntt) = (self.msm_min_k, self.ntt_min_k);
        if self.msm_source == self.ntt_source {
            write!(
                f,
                "{}: MSM crossover K{msm}, NTT crossover K{ntt}",
                self.msm_source
            )
        } else {
            write!(
                f,
                "MSM crossover K{msm} ({}), NTT crossover K{ntt} ({})",
                self.msm_source, self.ntt_source
            )
        }
    }
}

/// Per-line result of [`init_gpu_backend`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuInitStatus {
    /// midnight-proofs 0.7.x (feature `gpu-v1`): IR V0/V1, i.e. every built-in
    /// Zswap/Dust circuit and compactc's default zkir 2.0 contracts.
    pub v1: GpuLineStatus,
    /// midnight-proofs 0.8.x (feature `gpu-v2`): IR V2+ and zkir-v3.
    pub v2: GpuLineStatus,
    /// The 0.7.x line's crossovers; `None` unless `v1` is
    /// [`GpuLineStatus::Initialized`].
    pub v1_calibration: Option<GpuCalibration>,
    /// The 0.8.x line's crossovers; `None` unless `v2` is
    /// [`GpuLineStatus::Initialized`].
    pub v2_calibration: Option<GpuCalibration>,
}

impl GpuInitStatus {
    /// Each line with a human-readable name, for logging.
    pub fn lines(&self) -> [(&'static str, GpuLineStatus); 2] {
        [
            ("midnight-proofs 0.7.x (IR V0/V1)", self.v1),
            ("midnight-proofs 0.8.x (IR V2+)", self.v2),
        ]
    }

    /// [`lines`](Self::lines), with each line's calibration.
    pub fn lines_with_calibration(
        &self,
    ) -> [(&'static str, GpuLineStatus, Option<GpuCalibration>); 2] {
        let [(v1_name, v1), (v2_name, v2)] = self.lines();
        [
            (v1_name, v1, self.v1_calibration),
            (v2_name, v2, self.v2_calibration),
        ]
    }
}

/// One line's handle on the backend's autotune, reached through that line's
/// midnight-proofs fork. Both forks re-export the one backend crate, but this
/// crate names its types only through a fork's `gpu_accel` (either fork may be
/// compiled out), so `gpu_line_calibration!` adapts each path to this.
#[cfg(any(feature = "gpu-v1", feature = "gpu-v2"))]
struct GpuLineCalibration {
    /// The backend's `min_gpu_size` / `min_ntt_gpu_size`: the crossovers its
    /// prover routes by. Unless pinned by its env override, the first call to
    /// either, from whichever line, loads the calibration once per process:
    /// the persisted one if it matches this machine, else one it measures and
    /// persists. A later call from the other line reads the loaded values.
    msm_min_size: fn() -> usize,
    ntt_min_size: fn() -> usize,
    /// `calibration_report()`: env overrides and the persisted calibration.
    /// Never measures.
    report: fn() -> GpuCalibrationReport,
    /// `run_calibration(false)`: persists a calibration, measuring only if
    /// none matches this machine.
    ensure: fn() -> Result<GpuCalibrationReport, String>,
}

/// The parts of the backend's `CalibrationReport` used here.
#[cfg(any(feature = "gpu-v1", feature = "gpu-v2"))]
struct GpuCalibrationReport {
    msm_min_k: (u32, GpuCalibrationSource),
    ntt_min_k: (u32, GpuCalibrationSource),
    /// `MIDNIGHT_MSM_WINDOW` pins the MSM window, which then ignores the
    /// calibration.
    window_pinned: bool,
    /// A persisted calibration matches this machine.
    persisted: bool,
    gpu_available: bool,
    autotune_enabled: bool,
}

/// [`GpuLineCalibration`] for the midnight-proofs crate `$proofs`, whose GPU
/// fork re-exports the backend's autotune from `gpu_accel`.
#[cfg(any(feature = "gpu-v1", feature = "gpu-v2"))]
macro_rules! gpu_line_calibration {
    ($proofs:ident) => {{
        use $proofs::gpu_accel::{CalibrationReport, Provenance};
        fn convert(report: CalibrationReport) -> GpuCalibrationReport {
            let source = |provenance: Provenance| match provenance {
                Provenance::EnvOverride => GpuCalibrationSource::EnvOverride,
                Provenance::Calibrated => GpuCalibrationSource::Calibrated,
                Provenance::Fallback if report.autotune_enabled => GpuCalibrationSource::Fallback,
                Provenance::Fallback => GpuCalibrationSource::Disabled,
            };
            GpuCalibrationReport {
                msm_min_k: (report.msm_min_k.0, source(report.msm_min_k.1)),
                ntt_min_k: (report.ntt_min_k.0, source(report.ntt_min_k.1)),
                window_pinned: report.window_env.is_some(),
                persisted: report.cache_valid,
                gpu_available: report.gpu_available,
                autotune_enabled: report.autotune_enabled,
            }
        }
        GpuLineCalibration {
            msm_min_size: $proofs::gpu_accel::min_gpu_size,
            ntt_min_size: $proofs::gpu_accel::min_ntt_gpu_size,
            report: || convert($proofs::gpu_accel::calibration_report()),
            ensure: || $proofs::gpu_accel::run_calibration(false).map(convert),
        }
    }};
}

#[cfg(any(feature = "gpu-v1", feature = "gpu-v2"))]
impl GpuLineCalibration {
    /// Makes the calibration effective in the backend now, rather than inside
    /// this line's first proof, and reads back the crossovers this line's
    /// prover uses.
    fn make_effective(&self) -> GpuCalibration {
        // The backend's own lazy load (nothing is measured under
        // MIDNIGHT_AUTOTUNE=0).
        let msm_min_k = (self.msm_min_size)().trailing_zeros();
        let ntt_min_k = (self.ntt_min_size)().trailing_zeros();
        let report = (self.report)();
        // With both crossovers pinned by env, neither query reached the
        // calibration, but the MSM window still reads it, at the first MSM.
        // Persist one now, so that read loads a file instead of measuring
        // inside a proof.
        if report.autotune_enabled
            && report.gpu_available
            && !report.persisted
            && !report.window_pinned
            && report.msm_min_k.1 == GpuCalibrationSource::EnvOverride
            && report.ntt_min_k.1 == GpuCalibrationSource::EnvOverride
            && let Err(e) = (self.ensure)()
        {
            warn!("GPU autotune calibration failed: {e}");
        }
        GpuCalibration {
            msm_min_k,
            msm_source: effective_source(report.msm_min_k, msm_min_k),
            ntt_min_k,
            ntt_source: effective_source(report.ntt_min_k, ntt_min_k),
        }
    }
}

/// The source of the crossover a prover holds (`effective_k`), given the
/// report's view of it. The report reads the persisted file, the prover what
/// it loaded or measured; they disagree only if this process measured a
/// calibration it could not persist (an unwritable `MIDNIGHT_AUTOTUNE_CACHE`)
/// or another process rewrote the file since, so a mismatch is calibrated.
/// (A measured value that happens to equal the built-in default keeps the
/// report's word, which is also what [`autotune_cli`] `--show` prints.)
#[cfg(any(feature = "gpu-v1", feature = "gpu-v2"))]
fn effective_source(
    (reported_k, source): (u32, GpuCalibrationSource),
    effective_k: u32,
) -> GpuCalibrationSource {
    if effective_k == reported_k {
        source
    } else {
        GpuCalibrationSource::Calibrated
    }
}

/// Eagerly initializes the GPU proving line(s) compiled into this build
/// (features `gpu-v1` / `gpu-v2`) and reports each line's outcome separately.
/// Both lines link ONE instance of the CUDA backend crate: the backend load,
/// the device, the NTT domain and the calibration are process-wide and happen
/// once, on the first line initialized; the second line's initialization
/// reaches only the backend's typed layer for its own curves (its warm-up
/// MSM). The outcome is still per line, as the 0.7.x line proves every
/// built-in circuit, so one line coming up must not hide the other failing
/// to warm up.
///
/// Call once at process start-up, before any proving, so backend loading,
/// device selection, warm-up and calibration happen up front rather than
/// inside the first proof, and a misconfigured GPU deployment surfaces
/// immediately. With `MIDNIGHT_DEVICE=cpu` no GPU operation is attempted, so
/// nothing is initialized (every compiled-in line reports
/// [`GpuLineStatus::Skipped`]).
///
/// Calibration: the backend loads its GPU/CPU crossovers once per process, on
/// the first size check, from the file the autotune persisted for this
/// machine, or by measuring them (about a second) and persisting the result if
/// there is none. This function makes that happen here, right after the first
/// line that came up warmed up (the 0.7.x line, when both are compiled in);
/// the other line then reads the values already loaded. So nothing is measured
/// inside a proof, and both lines prove with the same crossovers, reported per
/// line. `MIDNIGHT_AUTOTUNE=0` turns calibration off (built-in defaults;
/// nothing is measured). `MIDNIGHT_AUTOTUNE=refresh` re-measures once, at that
/// first load; [`autotune_cli`] with `--refresh` does the same from the
/// command line. A line that failed to load or warm up is not calibrated, as
/// timings taken on it would be meaningless.
pub fn init_gpu_backend() -> GpuInitStatus {
    #[cfg(feature = "gpu-v1")]
    let (v1, v1_calibration) = init_gpu_line(
        midnight_proofs_v1::init_gpu_backend,
        gpu_line_calibration!(midnight_proofs_v1),
    );
    #[cfg(not(feature = "gpu-v1"))]
    let (v1, v1_calibration) = (GpuLineStatus::NotCompiled, None);
    // After the 0.7.x line: the calibration it loaded (once per process) is
    // what this line reads back (see above).
    #[cfg(feature = "gpu-v2")]
    let (v2, v2_calibration) = init_gpu_line(
        midnight_proofs::init_gpu_backend,
        gpu_line_calibration!(midnight_proofs),
    );
    #[cfg(not(feature = "gpu-v2"))]
    let (v2, v2_calibration) = (GpuLineStatus::NotCompiled, None);
    GpuInitStatus {
        v1,
        v2,
        v1_calibration,
        v2_calibration,
    }
}

/// Initializes one line and, if it came up, makes its calibration effective.
#[cfg(any(feature = "gpu-v1", feature = "gpu-v2"))]
fn init_gpu_line(
    line_init: fn() -> Option<std::time::Duration>,
    calibration: GpuLineCalibration,
) -> (GpuLineStatus, Option<GpuCalibration>) {
    if cpu_device_selected() {
        return (GpuLineStatus::Skipped, None);
    }
    match line_init() {
        Some(warmup) => (
            GpuLineStatus::Initialized(warmup),
            Some(calibration.make_effective()),
        ),
        None => (GpuLineStatus::Failed, None),
    }
}

/// Whether `MIDNIGHT_DEVICE` selects CPU-only proving, parsed as the GPU
/// backend does (`DeviceType::from_env` in midnight-bls12-381-cuda: `cpu` or
/// `blst`, any case).
#[cfg(any(feature = "gpu-v1", feature = "gpu-v2"))]
fn cpu_device_selected() -> bool {
    std::env::var("MIDNIGHT_DEVICE")
        .is_ok_and(|device| matches!(device.to_lowercase().as_str(), "cpu" | "blst"))
}

/// The GPU backend's autotune command (`midnight-gpu-autotune`), for a binary
/// to mount as a subcommand: `program` is the command name its usage text
/// shows, `args` the arguments after it, and the result the process exit
/// code. Flags, output and exit codes are the standalone command's:
///
/// ```text
/// (none)     use the persisted calibration if it matches, else measure + persist
/// --refresh  re-measure and overwrite it
/// --show     print the effective values and their sources; never measures
/// --quiet    stable key=value output, combined with the above
/// ```
///
/// Exit codes: `0` calibrated values in effect, `2` no GPU or
/// `MIDNIGHT_AUTOTUNE=0` (built-in defaults in effect), `1` failure.
///
/// Both lines share one backend, which reads one file
/// (`MIDNIGHT_AUTOTUNE_CACHE`, an absolute path, else `autotune-v1.conf` in
/// `$XDG_CACHE_HOME/midnight-gpu` or `~/.cache/midnight-gpu`), so this reaches
/// the command through the 0.7.x line's fork (the 0.8.x line's in a build with
/// `gpu-v2` alone); it is the same command either way. In a build without
/// either it prints why to stderr and returns 2, as for a missing GPU.
pub fn autotune_cli<I, S>(program: &str, args: I) -> u8
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    #[cfg(feature = "gpu-v1")]
    {
        midnight_proofs_v1::gpu_accel::autotune_cli(program, args)
    }
    #[cfg(all(feature = "gpu-v2", not(feature = "gpu-v1")))]
    {
        midnight_proofs::gpu_accel::autotune_cli(program, args)
    }
    #[cfg(not(any(feature = "gpu-v1", feature = "gpu-v2")))]
    {
        drop(args);
        eprintln!("{program}: this build has no GPU support (enable the `gpu` feature)");
        2
    }
}

/// Implements `ProvingProvider` locally, dispatching V0/V1 circuits through
/// the old (zk-stdlib v1) pipeline and V2+ through the current pipeline.
pub struct LocalProvingProvider<'a, R: Rng + CryptoRng + SplittableRng, S, P> {
    /// The randomness to use for proving
    pub rng: R,
    /// The resolver to use to fetch keys
    pub resolver: &'a S,
    /// The parameters provider to use
    pub params: &'a P,
}

impl<
    'a,
    R: Rng + CryptoRng + SplittableRng,
    S: transient_crypto::proofs::Resolver,
    P: transient_crypto::proofs::ParamsProverProvider,
> transient_crypto::proofs::ProvingProvider for LocalProvingProvider<'a, R, S, P>
{
    async fn check(
        &self,
        preimage: &transient_crypto::proofs::ProofPreimage,
    ) -> Result<Vec<Option<usize>>, anyhow::Error> {
        let proving_data = self
            .resolver
            .resolve_key(preimage.key_location.clone())
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "attempted to check proof for '{}' without circuit data!",
                    preimage.key_location.0
                )
            })?;
        let ir = IrSource::load_from_tagged(std::io::Cursor::new(&proving_data.ir_source[..]))?;
        use transient_crypto::proofs::Zkir;
        ir.check(preimage)
    }

    async fn prove(
        self,
        preimage: &transient_crypto::proofs::ProofPreimage,
        overwrite_binding_input: Option<transient_crypto::curve::Fr>,
    ) -> Result<Proof, anyhow::Error> {
        let mut preimage = preimage.clone();
        if let Some(binding_input) = overwrite_binding_input {
            preimage.binding_input = binding_input;
        }

        // Resolve to determine the IR version.
        let proving_data = self
            .resolver
            .resolve_key(preimage.key_location.clone())
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "attempted to prove '{}' without circuit data!",
                    preimage.key_location.0
                )
            })?;
        let ir = IrSource::load_from_tagged(std::io::Cursor::new(&proving_data.ir_source[..]))?;

        match ir.version {
            IrMinorVersion::V0 | IrMinorVersion::V1 => {
                // V0/V1: use old Zkir pipeline directly.
                // Load PK via IrSource's multi-tag loader, then convert to old ProverKey.
                use transient_crypto::proofs::Zkir as _;
                let current_pk = IrSource::load_prover_key_from_tagged(std::io::Cursor::new(
                    &proving_data.prover_key[..],
                ))?;
                let mut pk_buf = Vec::new();
                serialize::Serializable::serialize(&current_pk, &mut pk_buf)?;
                let pk: transient_crypto_old::proofs::ProverKey<IrSource> =
                    serialize::Deserializable::deserialize(&mut &pk_buf[..], 0)?;
                let old_preimage = ir_v1::preimage_to_v1(&preimage);
                let v1_params = ir_v1::V1Params(self.params);
                #[cfg_attr(not(feature = "gpu-v1"), allow(unused_variables))]
                let (proof, pis, _) = transient_crypto_old::proofs::Zkir::prove(
                    &ir,
                    self.rng,
                    &v1_params,
                    pk,
                    &old_preimage,
                )
                .await?;
                // `ProofPreimage::prove` (the V2+ arm below, and every circuit on
                // Ledger 8) verifies each proof it makes; this direct call does
                // not. GPU builds restore that check, so a wrong GPU proof fails
                // here rather than as a transaction rejected on submission.
                #[cfg(feature = "gpu-v1")]
                ir_v1::self_verify(&proving_data.verifier_key, &proof, &pis).inspect_err(|e| {
                    error!(error = ?e, key = %preimage.key_location.0, "self-verification failed! This may be a bug, check that your keys match!");
                })?;
                Ok(Proof(proof.0))
            }
            _ => {
                // V2+: use the current pipeline.
                let (proof, _) = preimage
                    .prove::<IrSource>(self.rng, self.params, self.resolver)
                    .await?;
                Ok(proof)
            }
        }
    }

    fn split(&mut self) -> Self {
        Self {
            rng: self.rng.split(),
            resolver: self.resolver,
            params: self.params,
        }
    }

    fn resolver(&self) -> &impl transient_crypto::proofs::Resolver {
        self.resolver
    }
}

impl<
    'a,
    R: Rng + CryptoRng + SplittableRng,
    S: transient_crypto_old::proofs::Resolver,
    P: transient_crypto_old::proofs::ParamsProverProvider,
> transient_crypto_old::proofs::ProvingProvider for LocalProvingProvider<'a, R, S, P>
{
    async fn check(
        &self,
        preimage: &transient_crypto_old::proofs::ProofPreimage,
    ) -> Result<Vec<Option<usize>>, anyhow::Error> {
        let proving_data = self
            .resolver
            .resolve_key(preimage.key_location.clone())
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "attempted to check proof for '{}' without circuit data!",
                    preimage.key_location.0
                )
            })?;
        let ir = IrSource::load_from_tagged(std::io::Cursor::new(&proving_data.ir_source[..]))?;
        use transient_crypto_old::proofs::Zkir as V1Zkir;
        ir.check(preimage)
    }

    async fn prove(
        self,
        preimage: &transient_crypto_old::proofs::ProofPreimage,
        overwrite_binding_input: Option<transient_crypto_old::curve::Fr>,
    ) -> Result<transient_crypto_old::proofs::Proof, anyhow::Error> {
        let mut preimage = preimage.clone();
        if let Some(binding_input) = overwrite_binding_input {
            preimage.binding_input = binding_input;
        }

        let (proof, _) = preimage
            .prove::<IrSource>(self.rng, self.params, self.resolver)
            .await?;
        Ok(proof)
    }

    fn split(&mut self) -> Self {
        Self {
            rng: self.rng.split(),
            resolver: self.resolver,
            params: self.params,
        }
    }
}
