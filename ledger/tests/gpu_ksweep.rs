// This file is part of midnight-ledger.
// Copyright (C) Midnight Foundation
// SPDX-License-Identifier: Apache-2.0
//
// Ledger-level GPU proving k-sweep.
//
// Runs at the midnight-ledger level (this crate's test suite, the ledger's integrated
// build: the patched GPU fork + the ledger's own ICICLE build + our CUDA backend .so),
// driving the EXACT prover seam the ledger uses — `midnight_proofs::plonk::create_proof`
// with the KZG commitment scheme. Because the real ledger circuits (zswap/dust) top out
// at k~=15, this uses a synthetic domain-filling circuit so we can sweep k = 14..19 and
// see where GPU beats CPU (the crossover) and how the gap scales.
//
// Backend is chosen per-process (config is OnceLock-frozen), same as the real-tx bench:
//   MIDNIGHT_DEVICE   = cpu | gpu
//   ICICLE_BACKEND_INSTALL_DIR = ~/.local/icicle-backends/ours   (gpu arm)
//   MIDNIGHT_GPU_STRICT = 1   (gpu arm: panic on any silent CPU fallback)
// Tunables:
//   KSWEEP_KS      (default "14,15,16,17,18,19")
//   KSWEEP_REPEAT  (default 3)   timed proves per k; the MEDIAN is reported
//
// Run (from midnight-ledger/):
//   BIN=target/release/deps/gpu_ksweep-*
//   LD_LIBRARY_PATH=$PWD/target/release/deps/icicle/lib \
//   MIDNIGHT_DEVICE=gpu MIDNIGHT_GPU_STRICT=1 \
//   ICICLE_BACKEND_INSTALL_DIR=~/.local/icicle-backends/ours \
//     ./$BIN --exact gpu_ksweep_bench --ignored --nocapture

use blake2b_simd::State as Blake2bState;
use midnight_curves::{Bls12, Fq};
use midnight_proofs::{
    circuit::{Layouter, SimpleFloorPlanner, Value},
    plonk::{
        create_proof as create_plonk_proof, keygen_pk, keygen_vk, prepare as prepare_plonk_proof,
        Advice, Circuit, Column, Constraints, ConstraintSystem, Error, Selector,
    },
    poly::{
        commitment::Guard,
        kzg::{params::ParamsKZG, KZGCommitmentScheme},
        Rotation,
    },
    transcript::{CircuitTranscript, Transcript},
};
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use std::time::Instant;

/// Domain-filling circuit: 8 advice columns filled over half the domain, so polynomial
/// sizes equal the domain size and the GPU NTT (>=2^12) / MSM (>=2^13) paths are exercised.
/// (Ported from the fork's gpu_correctness e2e harness so the ledger-level numbers are
/// directly comparable to the fork-level ones.)
#[derive(Clone)]
struct DomainFillingCircuit {
    k: u32,
    fill_ratio: f64,
}

#[derive(Clone, Debug)]
struct DomainFillingConfig {
    advice_cols: Vec<Column<Advice>>,
    selector: Selector,
}

impl Circuit<Fq> for DomainFillingCircuit {
    type Config = DomainFillingConfig;
    type FloorPlanner = SimpleFloorPlanner;
    // circuit-params is always ON in the ledger build (enabled by midnight-circuits +
    // midnight-zk-stdlib), so this associated type is declared unconditionally.
    type Params = ();

    fn without_witnesses(&self) -> Self {
        self.clone()
    }

    fn configure(meta: &mut ConstraintSystem<Fq>) -> Self::Config {
        let advice_cols: Vec<_> = (0..8).map(|_| meta.advice_column()).collect();
        let selector = meta.selector();
        meta.create_gate("domain_fill", |vc| {
            let a0 = vc.query_advice(advice_cols[0], Rotation::cur());
            Constraints::with_selector(selector, vec![a0.clone() - a0])
        });
        DomainFillingConfig {
            advice_cols,
            selector,
        }
    }

    fn synthesize(
        &self,
        config: Self::Config,
        mut layouter: impl Layouter<Fq>,
    ) -> Result<(), Error> {
        layouter.assign_region(
            || "domain_fill_region",
            |mut region| {
                let domain_size = 1usize << self.k;
                let rows_to_fill = ((domain_size as f64) * self.fill_ratio) as usize;
                for row in 0..rows_to_fill {
                    config.selector.enable(&mut region, row)?;
                    for (col_idx, col) in config.advice_cols.iter().enumerate() {
                        region.assign_advice(
                            || format!("a[{col_idx}][{row}]"),
                            *col,
                            row,
                            || Value::known(Fq::from((row + col_idx + 1) as u64)),
                        )?;
                    }
                }
                Ok(())
            },
        )
    }
}

fn median(mut xs: Vec<f64>) -> f64 {
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = xs.len();
    if n == 0 {
        0.0
    } else if n % 2 == 1 {
        xs[n / 2]
    } else {
        (xs[n / 2 - 1] + xs[n / 2]) / 2.0
    }
}

fn env_list_u32(key: &str, default: &[u32]) -> Vec<u32> {
    std::env::var(key)
        .ok()
        .map(|s| {
            s.split(',')
                .filter_map(|t| t.trim().parse().ok())
                .collect::<Vec<u32>>()
        })
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_vec())
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Per-k CPU-or-GPU proving benchmark (whichever backend this process is configured for).
/// Reports keygen / prove(median) / verify ms and proof size for each k.
#[test]
#[ignore = "benchmark; run explicitly with --ignored --nocapture and MIDNIGHT_DEVICE set"]
fn gpu_ksweep_bench() {
    let ks = env_list_u32("KSWEEP_KS", &[14, 15, 16, 17, 18, 19]);
    let repeat = env_usize("KSWEEP_REPEAT", 3);
    let device = std::env::var("MIDNIGHT_DEVICE").unwrap_or_else(|_| "auto".into());
    let strict = std::env::var("MIDNIGHT_GPU_STRICT").unwrap_or_else(|_| "0".into());
    let backend = std::env::var("ICICLE_BACKEND_INSTALL_DIR").unwrap_or_else(|_| "<default>".into());

    eprintln!("== gpu_ksweep_bench :: device={device} strict={strict} repeat={repeat} ==");
    eprintln!("   backend_dir={backend}");
    eprintln!(
        "{:>4} {:>11} {:>13} {:>11} {:>10}",
        "k", "keygen_ms", "prove_ms(med)", "verify_ms", "bytes"
    );

    for k in ks {
        let mut rng = ChaCha8Rng::seed_from_u64(42 + k as u64);
        let params = ParamsKZG::<Bls12>::unsafe_setup(k, &mut rng);

        // keygen (once)
        let t = Instant::now();
        let vk = keygen_vk::<_, KZGCommitmentScheme<Bls12>, _>(
            &params,
            &DomainFillingCircuit { k, fill_ratio: 0.5 },
        )
        .expect("keygen_vk");
        let pk = keygen_pk(vk, &DomainFillingCircuit { k, fill_ratio: 0.5 }).expect("keygen_pk");
        let keygen_ms = t.elapsed().as_secs_f64() * 1e3;

        // prove (repeat -> median).  committed-instances is always ON in the ledger build,
        // so nb_committed_instances (the `0`) is passed unconditionally.
        let mut prove_times = Vec::with_capacity(repeat);
        let mut proof = Vec::new();
        for _ in 0..repeat {
            let circuit = DomainFillingCircuit { k, fill_ratio: 0.5 };
            let t = Instant::now();
            let mut transcript = CircuitTranscript::<Blake2bState>::init();
            create_plonk_proof::<_, KZGCommitmentScheme<Bls12>, _, _>(
                &params,
                &pk,
                &[circuit],
                0,
                &[&[]],
                &mut rng,
                &mut transcript,
            )
            .expect("create_proof");
            proof = transcript.finalize();
            prove_times.push(t.elapsed().as_secs_f64() * 1e3);
        }
        let prove_ms = median(prove_times);

        // verify (CPU/BLST pairing check of whatever backend produced the proof)
        let t = Instant::now();
        let mut vt = CircuitTranscript::<Blake2bState>::init_from_bytes(&proof);
        let guard = prepare_plonk_proof::<_, KZGCommitmentScheme<Bls12>, _>(
            pk.get_vk(),
            &[&[]],
            &[&[]],
            &mut vt,
        )
        .expect("prepare");
        assert!(
            guard.verify(&params.verifier_params()).is_ok(),
            "proof failed to verify at k={k}"
        );
        let verify_ms = t.elapsed().as_secs_f64() * 1e3;

        eprintln!(
            "{k:>4} {keygen_ms:>11.1} {prove_ms:>13.1} {verify_ms:>11.1} {:>10}",
            proof.len()
        );
    }
}
