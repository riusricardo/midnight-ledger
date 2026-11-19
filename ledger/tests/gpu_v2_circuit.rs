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

//! A real circuit on the 0.8.x line, which no transaction of `gpu_benchmark`
//! reaches: an IR V2 (zkir 2.2) program of in-circuit elliptic-curve
//! multiplications and additions and a hash, keygen'd from the IR and proved
//! with a seeded rng through zkir, as the proof server proves a contract
//! compiled to zkir 2.2.
//!
//! The proof bytes must equal the CPU's, pinned below as a blake2b fingerprint
//! taken from a build without `gpu` (the ledger's test builds fill spare lookup
//! rows in value order, so the bytes are reproducible), in every device mode,
//! and the verifying key must accept the proof. In a `gpu` build the proof
//! must also have run where the device mode says: GPU MSMs and NTTs on the
//! 0.8.x line and no fallback in forced GPU and Auto mode (the circuit is above
//! the crossovers), and a backend never loaded in CPU mode.
//!
//!   MIDNIGHT_PP=<dir of bls_midnight_2p<k>> [MIDNIGHT_DEVICE=gpu|auto|cpu MIDNIGHT_GPU_STRICT=1] \
//!     cargo test --release -p midnight-ledger-v9 --features proving,test-utilities[,gpu] \
//!     --test gpu_v2_circuit -- --nocapture
#![cfg(any(feature = "gpu", feature = "test-utilities"))]

use std::borrow::Cow;
use std::fs::File;
use std::io::BufReader;

use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use transient_crypto::curve::EmbeddedGroupAffine;
use transient_crypto::proofs::{
    KeyLocation, PARAMS_VERIFIER, ParamsProver, ParamsProverProvider, ProofPreimage, Zkir,
};
use zkir_v2::IrSource;

/// blake2b-256 of the proof a build without `gpu` makes.
const PROOF_BLAKE2B: &str = "02b6a3ad974da0220e507efeafc225770b2ef09cf844ac12b057017a8d25fd17";

/// In-circuit multiplications of the chain: enough rows for k = 14, as the real
/// transaction's circuits, above the MSM and NTT crossovers.
const MULS: usize = 24;

/// Inputs: a point (x, y) and two scalars. The point is multiplied MULS times
/// in a chain, alternately by each scalar, then added to the generator times
/// the second scalar, and the sum's coordinates are hashed. Each `ec_*`
/// defines the two coordinates of its result, the hash one value.
fn ir() -> String {
    let mut instructions = Vec::new();
    let (mut x, mut next) = (0, 4);
    for i in 0..MULS {
        instructions.push(format!(r#"{{ "op": "ec_mul", "a_x": {x}, "a_y": {}, "scalar": {} }}"#, x + 1, 2 + i % 2));
        (x, next) = (next, next + 2);
    }
    instructions.push(r#"{ "op": "ec_mul_generator", "scalar": 3 }"#.to_string());
    instructions.push(format!(r#"{{ "op": "ec_add", "a_x": {x}, "a_y": {}, "b_x": {next}, "b_y": {} }}"#, x + 1, next + 1));
    instructions.push(format!(r#"{{ "op": "transient_hash", "inputs": [{}, {}] }}"#, next + 2, next + 3));
    format!(
        r#"{{ "version": {{ "major": 2, "minor": 2 }}, "num_inputs": 4, "do_communications_commitment": false,
             "instructions": [{}] }}"#,
        instructions.join(", ")
    )
}

struct Params;

impl ParamsProverProvider for Params {
    async fn get_params(&self, k: u8) -> std::io::Result<ParamsProver> {
        let dir = std::env::var("MIDNIGHT_PP").expect("MIDNIGHT_PP: the directory of bls_midnight_2p<k>");
        ParamsProver::read(BufReader::new(File::open(format!("{dir}/bls_midnight_2p{k}"))?))
    }
}

/// GPU MSMs, GPU NTTs and fallbacks of the 0.8.x line so far.
#[cfg(feature = "gpu")]
fn line_work() -> [u64; 3] {
    use midnight_proofs::gpu_accel::backend::{
        config::gpu_fallback_count,
        diag::{OpKind, gpu_op_count},
        line_id,
    };
    [gpu_op_count(line_id(), OpKind::Msm), gpu_op_count(line_id(), OpKind::Ntt), gpu_fallback_count()]
}

#[tokio::test]
async fn v2_circuit_proves_the_cpu_bytes() {
    #[cfg(feature = "gpu")]
    let device = midnight_proofs::gpu_accel::backend::config::device_type();
    #[cfg(feature = "gpu")]
    eprintln!("[v2-circuit] device={device}: {:?}", zkir_v2::init_gpu_backend());

    let ir = IrSource::load(ir().as_bytes()).expect("the IR loads");
    let (pk, vk) = ir.v2_keygen(&Params).await.expect("keygen");
    let p = EmbeddedGroupAffine::generator();
    let preimage = ProofPreimage {
        binding_input: 42.into(),
        communications_commitment: None,
        inputs: vec![p.x().unwrap(), p.y().unwrap(), 42.into(), 63.into()],
        private_transcript: vec![],
        public_transcript_inputs: vec![],
        public_transcript_outputs: vec![],
        key_location: KeyLocation(Cow::Borrowed("builtin")),
    };

    #[cfg(feature = "gpu")]
    let before = line_work();
    let (proof, pis, _skips) = ir
        .prove(&mut ChaCha20Rng::from_seed([42; 32]), &Params, pk, &preimage)
        .await
        .expect("prove");
    #[cfg(feature = "gpu")]
    let after = line_work();

    vk.verify(&PARAMS_VERIFIER, &proof, pis.into_iter()).expect("the verifying key accepts the proof");
    let digest = blake2b_simd::Params::new().hash_length(32).hash(&proof.0).to_hex();
    eprintln!("[v2-circuit] k={} proof {} bytes, blake2b {}", ir.k(), proof.0.len(), digest.as_str());
    assert_eq!(digest.as_str(), PROOF_BLAKE2B, "the proof bytes differ from the CPU's");

    #[cfg(feature = "gpu")]
    if device == midnight_proofs::gpu_accel::backend::config::DeviceType::Cpu {
        use midnight_proofs::gpu_accel::backend::diag::backend_load_count;
        assert_eq!(backend_load_count(), 0, "CPU mode loaded the GPU backend");
    } else {
        use midnight_proofs::gpu_accel::backend::config::{min_gpu_size, min_ntt_gpu_size};
        let [msm, ntt, fallbacks] = [0, 1, 2].map(|i| after[i] - before[i]);
        assert_eq!(fallbacks, 0, "a GPU operation fell back to the CPU");
        // Auto routes by the crossovers this machine measured; forced GPU mode always.
        use midnight_proofs::gpu_accel::backend::config::DeviceType;
        let routed = device == DeviceType::Gpu || 1usize << ir.k() >= min_gpu_size().max(min_ntt_gpu_size());
        if routed {
            assert!(msm > 0 && ntt > 0, "the 0.8.x line launched {msm} GPU MSMs and {ntt} GPU NTTs");
        } else {
            eprintln!("[v2-circuit] k={} is below this machine's crossovers: Auto proves it on the CPU", ir.k());
        }
    }
}
