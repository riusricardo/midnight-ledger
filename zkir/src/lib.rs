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

/// Per-line result of [`init_gpu_backend`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuInitStatus {
    /// midnight-proofs 0.7.x (feature `gpu-v1`): IR V0/V1, i.e. every built-in
    /// Zswap/Dust circuit and compactc's default zkir 2.0 contracts.
    pub v1: GpuLineStatus,
    /// midnight-proofs 0.8.x (feature `gpu-v2`): IR V2+ and zkir-v3.
    pub v2: GpuLineStatus,
}

impl GpuInitStatus {
    /// Each line with a human-readable name, for logging.
    pub fn lines(&self) -> [(&'static str, GpuLineStatus); 2] {
        [
            ("midnight-proofs 0.7.x (IR V0/V1)", self.v1),
            ("midnight-proofs 0.8.x (IR V2+)", self.v2),
        ]
    }
}

/// Eagerly initializes the GPU proving backend(s) compiled into this build
/// (features `gpu-v1` / `gpu-v2`) and reports each line's outcome separately:
/// the two lines link separate backend instances, and the 0.7.x one proves
/// every built-in circuit, so one line coming up must not hide the other
/// failing.
///
/// Call once at process start-up, before any proving, so backend loading,
/// device selection and warm-up happen up front rather than inside the first
/// proof, and a misconfigured GPU deployment surfaces immediately. With
/// `MIDNIGHT_DEVICE=cpu` no GPU operation is attempted, so nothing is
/// initialized (every compiled-in line reports [`GpuLineStatus::Skipped`]).
pub fn init_gpu_backend() -> GpuInitStatus {
    #[cfg(any(feature = "gpu-v1", feature = "gpu-v2"))]
    let init = |line_init: fn() -> Option<std::time::Duration>| {
        if cpu_device_selected() {
            GpuLineStatus::Skipped
        } else {
            line_init().map_or(GpuLineStatus::Failed, GpuLineStatus::Initialized)
        }
    };
    GpuInitStatus {
        #[cfg(feature = "gpu-v1")]
        v1: init(midnight_proofs_v1::init_gpu_backend),
        #[cfg(not(feature = "gpu-v1"))]
        v1: GpuLineStatus::NotCompiled,
        #[cfg(feature = "gpu-v2")]
        v2: init(midnight_proofs::init_gpu_backend),
        #[cfg(not(feature = "gpu-v2"))]
        v2: GpuLineStatus::NotCompiled,
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
