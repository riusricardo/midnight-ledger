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

/// Eagerly initializes the GPU proving backend(s) compiled into this build
/// (features `gpu-v1` / `gpu-v2`); a no-op returning `None` without them.
///
/// Call once at process start-up, before any proving, so backend loading,
/// device selection and warm-up happen up front rather than inside the first
/// proof, and a misconfigured GPU deployment surfaces immediately. Returns the
/// backend warm-up time when a backend was initialized.
pub fn init_gpu_backend() -> Option<std::time::Duration> {
    // midnight-proofs 0.7.x: the legacy (IR V0/V1) pipeline.
    #[cfg(feature = "gpu-v1")]
    let warmup = midnight_proofs_v1::init_gpu_backend();
    #[cfg(not(feature = "gpu-v1"))]
    let warmup = None;
    // midnight-proofs 0.8.x: the current (IR V2+, zkir-v3) pipeline, once the
    // 0.8.x GPU fork exists:
    // #[cfg(feature = "gpu-v2")]
    // let warmup = midnight_proofs::init_gpu_backend().or(warmup);
    warmup
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
                let (proof, _, _) = transient_crypto_old::proofs::Zkir::prove(
                    &ir,
                    self.rng,
                    &v1_params,
                    pk,
                    &old_preimage,
                )
                .await?;
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
