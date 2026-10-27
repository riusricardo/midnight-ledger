// This file is part of midnight-ledger.
// Copyright (C) Midnight Foundation
// SPDX-License-Identifier: Apache-2.0
//
// Ledger-level GPU proving k-sweep.
//
// Runs at the midnight-ledger level (this crate's test suite, the ledger's integrated
// build: the patched GPU forks + the ledger's own ICICLE build + our CUDA backend .so),
// driving the EXACT prover seam the ledger uses -- `midnight_proofs::plonk::create_proof`
// with the KZG commitment scheme -- on a synthetic domain-filling circuit, so k can be
// swept past the real circuits' sizes to find the GPU crossover and scaling.
//
// Ledger 9 links TWO midnight-proofs lines, so the sweep exists for each:
//   gpu_ksweep_bench     midnight-proofs 0.7.x (dev-dep `midnight-proofs-v1`): the legacy
//                        pipeline that proves the built-in Zswap/Dust circuits (V0 IR).
//   gpu_ksweep_bench_v2  midnight-proofs 0.8.x (dev-dep `midnight-proofs`): the current
//                        pipeline that proves Compact contract circuits (IR V2 / zkir v3).
// Both lines are redirected by the workspace [patch.crates-io]; each is GPU-accelerated
// only if its fork has the `gpu` feature enabled (zkir `gpu-v1` / `gpu-v2`).
//
// Backend is chosen per-process (config is OnceLock-frozen), same as the real-tx bench:
//   MIDNIGHT_DEVICE   = cpu | gpu | (unset => Auto thresholds)
//   ICICLE_BACKEND_INSTALL_DIR = ~/.local/icicle-backends/ours   (gpu arm)
//   MIDNIGHT_GPU_STRICT = 1   (gpu arm: panic on any silent CPU fallback)
// Unless MIDNIGHT_DEVICE=cpu, a sweep requires its line's GPU feature (`gpu` for the
// 0.7.x sweep, `gpu-v2` for the 0.8.x one) and MIDNIGHT_GPU_STRICT=1, initializes that
// line's backend before timing and panics if it did not come up, so a "gpu" table is
// never CPU numbers. Under Auto it also prints the line's effective MSM / NTT size
// thresholds, below which operations stay on CPU by design.
// Tunables:
//   KSWEEP_KS      (default "14,15,16,17,18,19")
//   KSWEEP_REPEAT  (default 3)   timed proves per k; the MEDIAN is reported
//
// Run one sweep per invocation, selected with --exact (`gpu_ksweep_bench` alone is a
// substring filter that also matches `gpu_ksweep_bench_v2`). The two sweeps also
// hold a shared lock, so an unfiltered `--ignored` runs them one after the other
// rather than on parallel test threads contending for the GPU. From midnight-ledger-v9/:
//   export MIDNIGHT_DEVICE=gpu MIDNIGHT_GPU_STRICT=1 \
//     ICICLE_BACKEND_INSTALL_DIR=~/.local/icicle-backends/ours
//   # 0.7.x (Zswap/Dust) line:
//   cargo test --release -p midnight-ledger-v9 --features proving,test-utilities,gpu \
//     --test gpu_ksweep -- --ignored --nocapture --exact gpu_ksweep_bench
//   # 0.8.x (contract) line:
//   cargo test --release -p midnight-ledger-v9 --features proving,test-utilities,gpu-v2 \
//     --test gpu_ksweep -- --ignored --nocapture --exact gpu_ksweep_bench_v2
// (Running the test binary directly also needs
//  LD_LIBRARY_PATH=$PWD/target/release/deps/icicle/lib.)

/// Held by each sweep for its whole run: libtest runs the selected tests on parallel
/// threads, and two sweeps sharing the GPU (and the CPU) would skew both tables.
static SWEEP_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

/// Whether `MIDNIGHT_DEVICE` selects the CPU, parsed as the GPU backend does.
fn cpu_device(device: &str) -> bool {
    matches!(device.to_lowercase().as_str(), "cpu" | "blst")
}

/// Whether `MIDNIGHT_DEVICE` forces the GPU (anything else but the CPU is Auto).
fn gpu_device(device: &str) -> bool {
    matches!(device.to_lowercase().as_str(), "gpu" | "cuda")
}

/// Whether `MIDNIGHT_GPU_STRICT` is on, parsed as the GPU backend does.
fn strict_mode(strict: &str) -> bool {
    matches!(strict.to_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Circuit + per-k benchmark, expanded once per midnight-proofs line. Expects the
/// expansion site to have `proofs` / `curves` in scope (the crate under test), a
/// `create_proof_kzg` shim (the two lines order create_proof's trailing args differently),
/// `GPU_FEATURE` and `gpu_backend_info`.
macro_rules! ksweep_impl {
    () => {
        use blake2b_simd::State as Blake2bState;
        use curves::{Bls12, Fq};
        use proofs::{
            circuit::{Layouter, SimpleFloorPlanner, Value},
            plonk::{
                keygen_pk, keygen_vk, prepare as prepare_plonk_proof, Advice, Circuit, Column,
                ConstraintSystem, Constraints, Error, Selector,
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

        /// Domain-filling circuit: 8 advice columns filled over half the domain, so
        /// polynomial sizes equal the domain size and the GPU NTT (>=2^12) / MSM (>=2^13)
        /// paths are exercised. (Ported from the fork's gpu_correctness e2e harness so the
        /// ledger-level numbers are directly comparable to the fork-level ones.)
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
            // circuit-params is always ON in the ledger build (enabled by midnight-circuits
            // + midnight-zk-stdlib on both lines), so this is declared unconditionally.
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

        /// Per-k CPU-or-GPU proving benchmark (whichever backend this process is
        /// configured for). Reports keygen / prove(median) / verify ms and proof size.
        pub(super) fn run(label: &str, ks: &[u32], repeat: usize) {
            let _serial = super::SWEEP_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let device = std::env::var("MIDNIGHT_DEVICE").unwrap_or_else(|_| "auto".into());
            let strict = std::env::var("MIDNIGHT_GPU_STRICT").unwrap_or_else(|_| "0".into());
            let backend = std::env::var("ICICLE_BACKEND_INSTALL_DIR")
                .unwrap_or_else(|_| "<default>".into());

            eprintln!(
                "== gpu_ksweep_bench [{label}] :: device={device} strict={strict} repeat={repeat} =="
            );
            eprintln!("   backend_dir={backend}");
            if !super::cpu_device(&device) {
                assert!(
                    GPU_FEATURE.1,
                    "MIDNIGHT_DEVICE={device} needs this line's GPU build: add --features {} \
                     (or set MIDNIGHT_DEVICE=cpu for a CPU sweep)",
                    GPU_FEATURE.0
                );
                // Without strict mode a GPU op that fails is redone on CPU with only a
                // [GPU-FALLBACK] line on stderr, and the table would still say gpu.
                assert!(
                    super::strict_mode(&strict),
                    "MIDNIGHT_DEVICE={device} needs MIDNIGHT_GPU_STRICT=1, so that a GPU \
                     operation that fails panics instead of silently running on CPU"
                );
                let warmup = proofs::init_gpu_backend();
                assert!(
                    warmup.is_some(),
                    "the {label} GPU backend did not initialize; see the [GPU-FALLBACK] / \
                     [GPU-WARN] line above"
                );
                eprintln!("   gpu backend initialized (warm-up {warmup:?})");
                if !super::gpu_device(&device) {
                    if let Some(info) = gpu_backend_info() {
                        eprintln!("   Auto: {info}");
                    }
                }
            }
            eprintln!(
                "{:>4} {:>11} {:>13} {:>11} {:>10}",
                "k", "keygen_ms", "prove_ms(med)", "verify_ms", "bytes"
            );

            for &k in ks {
                let mut rng = ChaCha8Rng::seed_from_u64(42 + k as u64);
                let params = ParamsKZG::<Bls12>::unsafe_setup(k, &mut rng);

                // keygen (once)
                let t = Instant::now();
                let vk = keygen_vk::<_, KZGCommitmentScheme<Bls12>, _>(
                    &params,
                    &DomainFillingCircuit { k, fill_ratio: 0.5 },
                )
                .expect("keygen_vk");
                let pk = keygen_pk(vk, &DomainFillingCircuit { k, fill_ratio: 0.5 })
                    .expect("keygen_pk");
                let keygen_ms = t.elapsed().as_secs_f64() * 1e3;

                // prove (repeat -> median)
                let mut prove_times = Vec::with_capacity(repeat);
                let mut proof = Vec::new();
                for _ in 0..repeat {
                    let circuit = DomainFillingCircuit { k, fill_ratio: 0.5 };
                    let t = Instant::now();
                    let mut transcript = CircuitTranscript::<Blake2bState>::init();
                    create_proof_kzg(&params, &pk, circuit, &mut rng, &mut transcript)
                        .expect("create_proof");
                    proof = transcript.finalize();
                    prove_times.push(t.elapsed().as_secs_f64() * 1e3);
                }
                let prove_ms = super::median(prove_times);

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
    };
}

/// midnight-proofs 0.7.x -- the Zswap/Dust (legacy IR) prover.
mod v1 {
    use midnight_curves_v1 as curves;
    use midnight_proofs_v1 as proofs;

    /// The ledger feature that GPU-accelerates this line, and whether it is on.
    const GPU_FEATURE: (&str, bool) = ("gpu", cfg!(feature = "gpu"));

    /// This line's GPU backend, with the MSM / NTT size thresholds that route each
    /// operation under Auto (`None` without this line's GPU feature).
    fn gpu_backend_info() -> Option<String> {
        #[cfg(feature = "gpu")]
        {
            use proofs::gpu_accel::GpuAccelerator;
            Some(proofs::gpu_accel::global_accelerator().backend_info())
        }
        #[cfg(not(feature = "gpu"))]
        None
    }

    // 0.7.x: create_proof(params, pk, circuits, nb_committed_instances, instances, rng, transcript).
    // committed-instances is always ON in the ledger build, so the `0` is unconditional.
    fn create_proof_kzg(
        params: &proofs::poly::kzg::params::ParamsKZG<curves::Bls12>,
        pk: &proofs::plonk::ProvingKey<
            curves::Fq,
            proofs::poly::kzg::KZGCommitmentScheme<curves::Bls12>,
        >,
        circuit: DomainFillingCircuit,
        rng: &mut rand_chacha::ChaCha8Rng,
        transcript: &mut proofs::transcript::CircuitTranscript<blake2b_simd::State>,
    ) -> Result<(), proofs::plonk::Error> {
        proofs::plonk::create_proof::<_, proofs::poly::kzg::KZGCommitmentScheme<curves::Bls12>, _, _>(
            params,
            pk,
            &[circuit],
            0,
            &[&[]],
            rng,
            transcript,
        )
    }

    ksweep_impl!();
}

/// midnight-proofs 0.8.x -- the Compact-contract (IR V2 / zkir v3) prover.
mod v2 {
    use midnight_curves as curves;
    use midnight_proofs as proofs;

    /// The ledger feature that GPU-accelerates this line, and whether it is on.
    const GPU_FEATURE: (&str, bool) = ("gpu-v2", cfg!(feature = "gpu-v2"));

    /// This line's GPU backend, with the MSM / NTT size thresholds that route each
    /// operation under Auto (`None` without this line's GPU feature).
    fn gpu_backend_info() -> Option<String> {
        #[cfg(feature = "gpu-v2")]
        {
            use proofs::gpu_accel::GpuAccelerator;
            Some(proofs::gpu_accel::global_accelerator().backend_info())
        }
        #[cfg(not(feature = "gpu-v2"))]
        None
    }

    // 0.8.x: create_proof(params, pk, circuits, nb_committed_instances, instances, transcript, rng)
    // -- the transcript and rng arguments swapped places relative to 0.7.x.
    fn create_proof_kzg(
        params: &proofs::poly::kzg::params::ParamsKZG<curves::Bls12>,
        pk: &proofs::plonk::ProvingKey<
            curves::Fq,
            proofs::poly::kzg::KZGCommitmentScheme<curves::Bls12>,
        >,
        circuit: DomainFillingCircuit,
        rng: &mut rand_chacha::ChaCha8Rng,
        transcript: &mut proofs::transcript::CircuitTranscript<blake2b_simd::State>,
    ) -> Result<(), proofs::plonk::Error> {
        proofs::plonk::create_proof::<_, proofs::poly::kzg::KZGCommitmentScheme<curves::Bls12>, _, _>(
            params,
            pk,
            &[circuit],
            0,
            &[&[]],
            transcript,
            rng,
        )
    }

    ksweep_impl!();
}

/// k-sweep on midnight-proofs 0.7.x (the line that proves the real Zswap/Dust tx).
#[test]
#[ignore = "benchmark; run with --ignored --nocapture --exact gpu_ksweep_bench and MIDNIGHT_DEVICE set"]
fn gpu_ksweep_bench() {
    let ks = env_list_u32("KSWEEP_KS", &[14, 15, 16, 17, 18, 19]);
    let repeat = env_usize("KSWEEP_REPEAT", 3);
    v1::run("midnight-proofs 0.7.x / v1 (Zswap+Dust)", &ks, repeat);
}

/// k-sweep on midnight-proofs 0.8.x (the line that proves Compact contract circuits).
#[test]
#[ignore = "benchmark; run with --ignored --nocapture --exact gpu_ksweep_bench_v2 and MIDNIGHT_DEVICE set"]
fn gpu_ksweep_bench_v2() {
    let ks = env_list_u32("KSWEEP_KS", &[14, 15, 16, 17, 18, 19]);
    let repeat = env_usize("KSWEEP_REPEAT", 3);
    v2::run("midnight-proofs 0.8.x / v2 (contracts)", &ks, repeat);
}
