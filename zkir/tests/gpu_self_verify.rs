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

//! GPU builds (`gpu-v1`): `LocalProvingProvider` verifies every V0/V1 proof it
//! makes against the circuit's verifier key, and fails the job when that does
//! not hold.
#![cfg(feature = "gpu-v1")]

use midnight_zkir::{IrSource, LocalProvingProvider};
use rand::SeedableRng;
use rand::rngs::StdRng;
use std::borrow::Cow;
use std::fs::File;
use std::io::BufReader;
use transient_crypto::proofs::{
    KeyLocation, Proof, ProofPreimage, ProvingKeyMaterial, ProvingProvider,
};

const DIR: &str = env!("MIDNIGHT_PP");

struct Params;

impl transient_crypto_old::proofs::ParamsProverProvider for Params {
    async fn get_params(
        &self,
        k: u8,
    ) -> std::io::Result<transient_crypto_old::proofs::ParamsProver> {
        transient_crypto_old::proofs::ParamsProver::read(BufReader::new(File::open(format!(
            "{DIR}/bls_midnight_2p{k}"
        ))?))
    }
}

impl transient_crypto::proofs::ParamsProverProvider for Params {
    async fn get_params(&self, k: u8) -> std::io::Result<transient_crypto::proofs::ParamsProver> {
        transient_crypto::proofs::ParamsProver::read(BufReader::new(File::open(format!(
            "{DIR}/bls_midnight_2p{k}"
        ))?))
    }
}

/// Serves one circuit's key material for every key location.
struct Keys(ProvingKeyMaterial);

impl transient_crypto::proofs::Resolver for Keys {
    async fn resolve_key(&self, _: KeyLocation) -> std::io::Result<Option<ProvingKeyMaterial>> {
        Ok(Some(self.0.clone()))
    }
}

/// Key material for a V0 circuit that asserts each of its `num_inputs` inputs.
async fn material(num_inputs: u32) -> ProvingKeyMaterial {
    let instructions = (0..num_inputs)
        .map(|i| format!(r#"{{ "op": "assert", "cond": {i} }}"#))
        .collect::<Vec<_>>()
        .join(", ");
    let ir = IrSource::load(
        format!(
            r#"{{
                "version": {{ "major": 2, "minor": 0 }},
                "num_inputs": {num_inputs},
                "do_communications_commitment": false,
                "instructions": [ {instructions} ]
            }}"#
        )
        .as_bytes(),
    )
    .unwrap();
    let (pk, vk) = transient_crypto_old::proofs::Zkir::keygen(&ir, &Params)
        .await
        .unwrap();
    let mut prover_key = Vec::new();
    IrSource::serialize_stdlib_v1_prover_key_to_tagged(ir.version, &pk, &mut prover_key).unwrap();
    let mut verifier_key = Vec::new();
    serialize::tagged_serialize(&vk, &mut verifier_key).unwrap();
    let mut ir_source = Vec::new();
    ir.serialize_to_tagged(&mut ir_source).unwrap();
    ProvingKeyMaterial {
        prover_key,
        verifier_key,
        ir_source,
    }
}

async fn prove(material: ProvingKeyMaterial) -> anyhow::Result<Proof> {
    let preimage = ProofPreimage {
        binding_input: 42.into(),
        communications_commitment: None,
        inputs: vec![1.into()],
        private_transcript: vec![],
        public_transcript_inputs: vec![],
        public_transcript_outputs: vec![],
        key_location: KeyLocation(Cow::Borrowed("builtin")),
    };
    let keys = Keys(material);
    LocalProvingProvider {
        rng: StdRng::seed_from_u64(42),
        resolver: &keys,
        params: &Params,
    }
    .prove(&preimage, None)
    .await
}

#[actix_rt::test]
async fn v0_proofs_are_self_verified() {
    let keys = material(1).await;
    prove(keys.clone())
        .await
        .expect("a proof with matching keys passes self-verification");

    // The same circuit proved again, but checked against another circuit's
    // verifier key: the job must fail rather than return the proof.
    let mismatched = ProvingKeyMaterial {
        verifier_key: material(2).await.verifier_key,
        ..keys
    };
    let err = prove(mismatched)
        .await
        .expect_err("a proof that does not verify must not be returned");
    assert!(err.to_string().contains("Invalid proof"), "{err:?}");
}
