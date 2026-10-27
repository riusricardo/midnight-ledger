// This file is part of midnight-ledger.
// Copyright (C) Midnight Foundation
// SPDX-License-Identifier: Apache-2.0
//
// One CUDA backend instance for both midnight-proofs lines, observed from the
// ledger's dependency graph.
//
// Ledger 9 links two midnight-proofs lines (0.7.x for the built-in Zswap/Dust
// circuits, 0.8.x for IR V2+ / zkir-v3 contracts), each a GPU fork that depends
// on the CUDA backend crate `midnight-bls12-381-cuda`. The backend's own
// dual-line test (its tests/dual_line.rs) shows that its two typed lines share
// one core state, but it reads the counters of the crate instance it links, so
// it can never see a SECOND instance in a consumer's graph. This test can: it
// reaches the backend through BOTH forks' re-export paths
// (`midnight_proofs_v1::gpu_accel::backend`, the `v02` line, and
// `midnight_proofs::gpu_accel::backend`, the `v03` line) and checks that they
// are one crate instance, i.e. one set of process-wide statics:
//   - at compile time: a function taken through one path has the type spelled
//     with the other path's types (two instances would be two sets of types,
//     and this a type error);
//   - at run time: `TypeId` equality of the backend's types through both paths,
//     line ids 0 (0.7.x / v02) and 1 (0.8.x / v03), and each line's dispatch
//     recognising its own curves types;
//   - the counters read THROUGH ONE FORK after the OTHER line's work ran show
//     that work, and after both lines proved on parallel threads both paths
//     report identical values: one backend load, one NTT domain
//     initialisation, at most one calibration, no GPU->CPU fallback, and GPU
//     MSMs and NTTs launched by both lines;
//   - every proof (synthetic domain-filling circuits at k = 13..15, the
//     built-in circuits' sizes, on both lines, proved on parallel threads)
//     passes the KZG pairing check with both MSMs evaluated by BLST, and each
//     line's GPU `commit` / `commit_lagrange` equals its BLST commitment byte
//     for byte.
//
// Needs the GPU and both lines' GPU features (`gpu-v2` implies `gpu`), forced
// to the GPU under strict mode so that every MSM and NTT is a GPU launch (what
// the counters count) and a failing one panics instead of running on the CPU.
// From midnight-ledger-v9/:
//   export MIDNIGHT_DEVICE=gpu MIDNIGHT_GPU_STRICT=1 \
//     ICICLE_BACKEND_INSTALL_DIR=~/.local/icicle-backends/ours
//   cargo test --release -p midnight-ledger-v9 --features gpu-v2 \
//     --test gpu_dual_line -- --test-threads=1 --nocapture
// (Running the test binary directly also needs
//  LD_LIBRARY_PATH=$PWD/target/release/deps/icicle/lib.) The counters are
// process-wide, so run with --test-threads=1: the GPU test must be the only
// GPU work in the process. MIDNIGHT_AUTOTUNE_CACHE relocates the calibration
// file (a fresh one is measured on the first run and read on the next).
#![cfg(feature = "gpu-v2")]

use std::any::TypeId;

/// The CUDA backend as the 0.7.x fork links it: its `v02` line (midnight-curves 0.2.x).
use midnight_proofs_v1::gpu_accel::backend as via_v1;
/// The CUDA backend as the 0.8.x fork links it: its `v03` line (midnight-curves 0.3.x).
use midnight_proofs::gpu_accel::backend as via_v2;

use via_v1::diag::OpKind;

/// Compile-time guard: `gpu_op_count` taken through the 0.8.x fork's path has
/// the type spelled with the `OpKind` reached through the 0.7.x fork's path. A
/// second backend crate instance in the graph would make those two distinct
/// types, and this a type error before any test runs.
#[allow(dead_code)]
const GPU_OP_COUNT_THROUGH_V2_TAKES_V1_OP_KIND: fn(usize, via_v1::diag::OpKind) -> u64 =
    via_v2::diag::gpu_op_count;

/// The same guard for the backend's error and device-bases types.
#[allow(dead_code)]
fn _backend_types_are_one_through_both_forks(
    e: via_v1::GpuError,
    b: via_v1::PrecomputedBases,
) -> (via_v2::GpuError, via_v2::PrecomputedBases) {
    (e, b)
}

/// Proof sizes, both lines: the built-in circuits' k range.
const KS: [u32; 3] = [13, 14, 15];
/// Size of the commitment identity check (a cached-bases GPU MSM).
const COMMIT_K: u32 = 14;

const KINDS: [OpKind; 4] = [OpKind::Msm, OpKind::MsmBatch, OpKind::Ntt, OpKind::NttBatch];

/// Every process-wide counter the backend exposes, as read through one fork's path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Counters {
    backend_loads: u64,
    ntt_domain_inits: u64,
    calibration_runs: u64,
    gpu_fallbacks: u64,
    /// GPU operations launched, `[line][kind]`: lines 0 (v02) and 1 (v03);
    /// kinds in `KINDS` order.
    ops: [[u64; 4]; 2],
}

macro_rules! read_counters {
    ($backend:ident) => {
        Counters {
            backend_loads: $backend::diag::backend_load_count(),
            ntt_domain_inits: $backend::diag::ntt_domain_init_count(),
            calibration_runs: $backend::diag::calibration_run_count(),
            gpu_fallbacks: $backend::config::gpu_fallback_count(),
            ops: [0usize, 1].map(|line| KINDS.map(|kind| $backend::diag::gpu_op_count(line, kind))),
        }
    };
}

impl Counters {
    fn through_v1() -> Self {
        read_counters!(via_v1)
    }

    fn through_v2() -> Self {
        read_counters!(via_v2)
    }

    /// Reads the counters through both forks' paths, which must agree exactly
    /// (nothing may be running on the GPU meanwhile).
    fn snapshot(when: &str) -> Self {
        let (v1, v2) = (Self::through_v1(), Self::through_v2());
        assert_eq!(
            v1, v2,
            "{when}: the counters read through midnight_proofs_v1 and through \
             midnight_proofs differ: two backend instances?"
        );
        v1
    }

    fn msm(&self, line: usize) -> u64 {
        self.ops[line][0]
    }

    fn ntt(&self, line: usize) -> u64 {
        self.ops[line][2]
    }

    fn print(&self, label: &str) {
        eprintln!(
            "   {label}: backend_loads={} ntt_domain_inits={} calibration_runs={} gpu_fallbacks={}",
            self.backend_loads, self.ntt_domain_inits, self.calibration_runs, self.gpu_fallbacks
        );
        for line in [0, 1] {
            eprintln!(
                "   {label}: line {line} ops: msm={} msm_batch={} ntt={} ntt_batch={}",
                self.ops[line][0], self.ops[line][1], self.ops[line][2], self.ops[line][3]
            );
        }
    }
}

/// Whether `MIDNIGHT_DEVICE` forces the GPU, parsed as the GPU backend does.
fn gpu_device(device: &str) -> bool {
    matches!(device.to_lowercase().as_str(), "gpu" | "cuda")
}

/// Whether `MIDNIGHT_GPU_STRICT` is on, parsed as the GPU backend does.
fn strict_mode(strict: &str) -> bool {
    matches!(strict.to_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

/// Circuit, BLST verification and the per-line checks, expanded once per
/// midnight-proofs line. Expects `proofs` / `curves` (the line's crates),
/// `NAME`, a `create_proof_kzg` shim (the two lines order create_proof's
/// trailing arguments differently) and a `split_guard` shim (their
/// `DualMSM::split` terms differ) in scope.
macro_rules! line_impl {
    () => {
        use blake2b_simd::State as Blake2bState;
        use curves::pairing::group::ff::Field as _;
        use curves::pairing::group::{Curve as _, Group as _, GroupEncoding as _};
        use curves::pairing::{MillerLoopResult as _, MultiMillerLoop as _};
        use curves::{Bls12, Fq, G1Projective, G2Prepared};
        use proofs::{
            circuit::{Layouter, SimpleFloorPlanner, Value},
            plonk::{
                keygen_pk, keygen_vk, prepare as prepare_plonk_proof, Advice, Circuit, Column,
                ConstraintSystem, Constraints, Error, Selector,
            },
            poly::{
                commitment::{Guard, PolynomialCommitmentScheme},
                kzg::{msm::DualMSM, params::ParamsKZG, KZGCommitmentScheme},
                EvaluationDomain, Rotation,
            },
            transcript::{CircuitTranscript, Transcript},
        };
        use rand::SeedableRng;
        use rand_chacha::ChaCha8Rng;

        /// Domain-filling circuit, as in gpu_ksweep.rs: 8 advice columns filled over
        /// half the domain, so polynomial sizes equal the domain size.
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

        /// The pairing check of `DualMSM::check`, with both MSMs evaluated by BLST
        /// (`G1Projective::multi_exp`) instead of `msm_specific`, which runs on the GPU
        /// under MIDNIGHT_DEVICE=gpu.
        fn verifies_on_blst(params: &ParamsKZG<Bls12>, guard: &DualMSM<Bls12>) -> bool {
            let [left, right] = split_guard(guard)
                .map(|(scalars, bases)| G1Projective::multi_exp(&bases, &scalars).to_affine());
            let s_g2 = G2Prepared::from(params.s_g2().to_affine());
            let n_g2 = G2Prepared::from((-params.g2()).to_affine());
            bool::from(
                Bls12::multi_miller_loop(&[(&left, &s_g2), (&right, &n_g2)])
                    .final_exponentiation()
                    .is_identity(),
            )
        }

        /// Proves the circuit at `k` with a seeded rng on this line's prover (GPU MSMs
        /// and NTTs under MIDNIGHT_DEVICE=gpu), checks the proof on BLST and with the
        /// library verifier, and returns its size in bytes. Thread-safe: everything it
        /// touches is its own.
        pub(super) fn prove_and_check(k: u32, seed: u64) -> usize {
            let mut rng = ChaCha8Rng::seed_from_u64(seed);
            let params = ParamsKZG::<Bls12>::unsafe_setup(k, &mut rng);
            let circuit = DomainFillingCircuit { k, fill_ratio: 0.5 };
            let vk = keygen_vk::<_, KZGCommitmentScheme<Bls12>, _>(&params, &circuit)
                .expect("keygen_vk");
            let pk = keygen_pk(vk, &circuit).expect("keygen_pk");
            let mut transcript = CircuitTranscript::<Blake2bState>::init();
            create_proof_kzg(&params, &pk, circuit, &mut rng, &mut transcript)
                .expect("create_proof");
            let proof = transcript.finalize();

            let mut vt = CircuitTranscript::<Blake2bState>::init_from_bytes(&proof);
            let guard = prepare_plonk_proof::<_, KZGCommitmentScheme<Bls12>, _>(
                pk.get_vk(),
                &[&[]],
                &[&[]],
                &mut vt,
            )
            .expect("prepare");
            assert!(
                verifies_on_blst(&params, &guard),
                "{NAME}: proof at k={k} failed the BLST pairing check"
            );
            assert!(
                guard.verify(&params.verifier_params()).is_ok(),
                "{NAME}: proof at k={k} failed to verify"
            );
            proof.len()
        }

        /// This line's GPU KZG commitments equal BLST's byte for byte: `commit_lagrange`
        /// of a random polynomial against `G1Projective::multi_exp` over the Lagrange
        /// bases, and `commit` of the same polynomial in coefficient form (through the
        /// domain's inverse NTT, a GPU NTT here) against the same reference.
        pub(super) fn commit_matches_blst(k: u32, seed: u64) {
            let mut rng = ChaCha8Rng::seed_from_u64(seed);
            let params = ParamsKZG::<Bls12>::unsafe_setup(k, &mut rng);
            let domain = EvaluationDomain::<Fq>::new(1, k);
            let mut lag = domain.empty_lagrange();
            for v in lag.iter_mut() {
                *v = Fq::random(&mut rng);
            }
            let scalars: Vec<Fq> = lag.iter().copied().collect();
            let blst = G1Projective::multi_exp(params.g_lagrange(), &scalars)
                .to_affine()
                .to_bytes();

            let gpu_lagrange = KZGCommitmentScheme::<Bls12>::commit_lagrange(&params, &lag)
                .to_affine()
                .to_bytes();
            assert_eq!(
                gpu_lagrange.as_ref(),
                blst.as_ref(),
                "{NAME}: GPU commit_lagrange differs from BLST at k={k}"
            );

            let coeff = domain.lagrange_to_coeff(lag);
            let gpu_coeff = KZGCommitmentScheme::<Bls12>::commit(&params, &coeff)
                .to_affine()
                .to_bytes();
            assert_eq!(
                gpu_coeff.as_ref(),
                blst.as_ref(),
                "{NAME}: GPU commit (coefficient form) differs from BLST at k={k}"
            );
        }
    };
}

/// midnight-proofs 0.7.x -- the Zswap/Dust (legacy IR) prover, backend line v02.
mod v1 {
    use midnight_curves_v1 as curves;
    use midnight_proofs_v1 as proofs;

    pub(super) const NAME: &str = "midnight-proofs 0.7.x / v1 (Zswap+Dust)";

    // 0.7.x: DualMSM::split yields (scalar, base) terms.
    fn split_guard(
        guard: &proofs::poly::kzg::msm::DualMSM<curves::Bls12>,
    ) -> [(Vec<curves::Fq>, Vec<curves::G1Projective>); 2] {
        let (left, right) = guard.split();
        [left, right].map(|terms| terms.into_iter().map(|(s, b)| (*s, *b)).unzip())
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

    line_impl!();
}

/// midnight-proofs 0.8.x -- the Compact-contract (IR V2 / zkir v3) prover, backend line v03.
mod v2 {
    use midnight_curves as curves;
    use midnight_proofs as proofs;

    pub(super) const NAME: &str = "midnight-proofs 0.8.x / v2 (contracts)";

    // 0.8.x: DualMSM::split yields (label, scalar, base) terms.
    fn split_guard(
        guard: &proofs::poly::kzg::msm::DualMSM<curves::Bls12>,
    ) -> [(Vec<curves::Fq>, Vec<curves::G1Projective>); 2] {
        let (left, right) = guard.split();
        [left, right].map(|terms| terms.into_iter().map(|(_, s, b)| (*s, *b)).unzip())
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

    line_impl!();
}

/// The static part of the single-instance claim, printed and asserted: the
/// backend's types are one type through both forks' paths, the lines carry
/// ids 0 and 1, and each line's dispatch recognises its own curves only.
fn check_one_crate_instance() {
    let op_kind = (
        TypeId::of::<via_v1::diag::OpKind>(),
        TypeId::of::<via_v2::diag::OpKind>(),
    );
    let bases = (
        TypeId::of::<via_v1::PrecomputedBases>(),
        TypeId::of::<via_v2::PrecomputedBases>(),
    );
    let error = (
        TypeId::of::<via_v1::GpuError>(),
        TypeId::of::<via_v2::GpuError>(),
    );
    eprintln!(
        "   TypeId through midnight_proofs_v1 == through midnight_proofs: OpKind {} \
         PrecomputedBases {} GpuError {}",
        op_kind.0 == op_kind.1,
        bases.0 == bases.1,
        error.0 == error.1
    );
    assert_eq!(op_kind.0, op_kind.1, "diag::OpKind is two types: two backend instances");
    assert_eq!(bases.0, bases.1, "PrecomputedBases is two types: two backend instances");
    assert_eq!(error.0, error.1, "GpuError is two types: two backend instances");

    let ids = (via_v1::line_id(), via_v2::line_id());
    eprintln!(
        "   line ids: midnight_proofs_v1 (0.7.x) -> {} ; midnight_proofs (0.8.x) -> {}",
        ids.0, ids.1
    );
    assert_eq!(ids.0, 0, "the 0.7.x fork must be wired to backend line v02 (id 0)");
    assert_eq!(ids.1, 1, "the 0.8.x fork must be wired to backend line v03 (id 1)");

    // Each line's dispatch answers by type identity for its own curves. (The
    // other line's types are deliberately not probed: the backend reports a
    // wrong-line type as a GPU fallback, which is a panic under strict mode;
    // the forks' `_backend_line_is_this_forks_curves` guards the wiring.)
    assert!(via_v1::is_fq::<midnight_curves_v1::Fq>());
    assert!(via_v1::is_g1_affine::<midnight_curves_v1::G1Affine>());
    assert!(via_v2::is_fq::<midnight_curves::Fq>());
    assert!(via_v2::is_g1_affine::<midnight_curves::G1Affine>());
    eprintln!("   dispatch: each line recognises its own curves types");
}

/// Static only (no GPU work): the backend reached through both forks is one
/// crate instance. Also part of the GPU test's output.
#[test]
fn backend_is_one_crate_instance_through_both_forks() {
    eprintln!("== gpu_dual_line :: static identity ==");
    check_one_crate_instance();
}

/// Both lines initialized, proving and verified in one process, with the
/// backend's process-wide counters observed through each fork's path.
#[test]
fn both_lines_prove_on_one_backend_instance() {
    let device = std::env::var("MIDNIGHT_DEVICE").unwrap_or_else(|_| "auto".into());
    let strict = std::env::var("MIDNIGHT_GPU_STRICT").unwrap_or_else(|_| "0".into());
    let backend_dir =
        std::env::var("ICICLE_BACKEND_INSTALL_DIR").unwrap_or_else(|_| "<default>".into());
    let cache = std::env::var("MIDNIGHT_AUTOTUNE_CACHE").unwrap_or_else(|_| "<default>".into());

    eprintln!("== gpu_dual_line :: device={device} strict={strict} ks={KS:?} ==");
    eprintln!("   backend_dir={backend_dir}");
    eprintln!("   autotune_cache={cache}");
    assert!(
        gpu_device(&device),
        "MIDNIGHT_DEVICE={device}: this test needs MIDNIGHT_DEVICE=gpu, so that every MSM \
         and NTT of both lines is a GPU launch (what the counters it checks count)"
    );
    assert!(
        strict_mode(&strict),
        "MIDNIGHT_DEVICE={device} needs MIDNIGHT_GPU_STRICT=1, so that a GPU operation \
         that fails panics instead of silently running on CPU"
    );

    check_one_crate_instance();

    // Initialize both lines as the proof server does. Nothing GPU-related has
    // run in this process before (the other test in this binary is static).
    let before_init = Counters::snapshot("before init");
    assert_eq!(
        before_init.backend_loads, 0,
        "the backend was loaded before this test initialized it"
    );
    let status = zkir_v2::init_gpu_backend();
    for (line, line_status, calibration) in status.lines_with_calibration() {
        let zkir_v2::GpuLineStatus::Initialized(warmup) = line_status else {
            panic!(
                "the GPU line {line} did not initialize ({line_status:?}); see the \
                 [GPU-FALLBACK] / [GPU-WARN] line above"
            );
        };
        eprintln!("   {line}: initialized (warm-up {warmup:?})");
        if let Some(calibration) = calibration {
            eprintln!("   {line}: calibration: {calibration}");
        }
    }
    assert_eq!(
        status.v1_calibration, status.v2_calibration,
        "one backend, one calibration: both lines must report the same crossovers"
    );
    let after_init = Counters::snapshot("after init");
    after_init.print("after init");
    assert_eq!(
        after_init.backend_loads, 1,
        "initializing both lines must load the ICICLE backend exactly once"
    );

    // Each line's GPU commitments against BLST.
    v1::commit_matches_blst(COMMIT_K, 7);
    eprintln!("   {}: GPU commit / commit_lagrange == BLST at k={COMMIT_K}", v1::NAME);
    v2::commit_matches_blst(COMMIT_K, 8);
    eprintln!("   {}: GPU commit / commit_lagrange == BLST at k={COMMIT_K}", v2::NAME);

    // Phase A: the 0.7.x line alone, its work observed through midnight_proofs
    // (the 0.8.x fork's path).
    let a0 = Counters::through_v2();
    let bytes = v1::prove_and_check(KS[0], 101);
    let a1 = Counters::through_v2();
    eprintln!(
        "   {} proved k={} ({bytes} bytes, verified); read through midnight_proofs: \
         line 0 msm +{} ntt +{}, line 1 msm +{} ntt +{}",
        v1::NAME,
        KS[0],
        a1.msm(0) - a0.msm(0),
        a1.ntt(0) - a0.ntt(0),
        a1.msm(1) - a0.msm(1),
        a1.ntt(1) - a0.ntt(1),
    );
    assert!(
        a1.msm(0) > a0.msm(0) && a1.ntt(0) > a0.ntt(0),
        "the 0.7.x line's GPU work is invisible through midnight_proofs' path: two backend \
         instances?"
    );
    assert_eq!(
        (a1.msm(1), a1.ntt(1)),
        (a0.msm(1), a0.ntt(1)),
        "the 0.7.x line's work moved the 0.8.x line's counters"
    );

    // Phase B: the 0.8.x line alone, observed through midnight_proofs_v1.
    let b0 = Counters::through_v1();
    let bytes = v2::prove_and_check(KS[0], 201);
    let b1 = Counters::through_v1();
    eprintln!(
        "   {} proved k={} ({bytes} bytes, verified); read through midnight_proofs_v1: \
         line 1 msm +{} ntt +{}, line 0 msm +{} ntt +{}",
        v2::NAME,
        KS[0],
        b1.msm(1) - b0.msm(1),
        b1.ntt(1) - b0.ntt(1),
        b1.msm(0) - b0.msm(0),
        b1.ntt(0) - b0.ntt(0),
    );
    assert!(
        b1.msm(1) > b0.msm(1) && b1.ntt(1) > b0.ntt(1),
        "the 0.8.x line's GPU work is invisible through midnight_proofs_v1's path: two \
         backend instances?"
    );
    assert_eq!(
        (b1.msm(0), b1.ntt(0)),
        (b0.msm(0), b0.ntt(0)),
        "the 0.8.x line's work moved the 0.7.x line's counters"
    );

    // Phase C: both lines on parallel threads, every k.
    let c0 = Counters::snapshot("before the parallel run");
    let proofs = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for (i, &k) in KS.iter().enumerate() {
            handles.push(scope.spawn(move || (0usize, k, v1::prove_and_check(k, 1000 + i as u64))));
            handles.push(scope.spawn(move || (1usize, k, v2::prove_and_check(k, 2000 + i as u64))));
        }
        let mut proofs = [0usize; 2];
        for handle in handles {
            let (line, k, bytes) = handle.join().expect("a proving thread panicked");
            eprintln!(
                "   parallel: {} k={k}: {bytes} bytes, verified",
                [v1::NAME, v2::NAME][line]
            );
            proofs[line] += 1;
        }
        proofs
    });
    eprintln!(
        "   proofs verified: {} on {}, {} on {} (plus one each above)",
        proofs[0],
        v1::NAME,
        proofs[1],
        v2::NAME
    );
    assert_eq!(proofs, [KS.len(), KS.len()]);

    let end = Counters::snapshot("after the parallel run");
    end.print("final (identical through both paths)");
    assert_eq!(
        end.backend_loads, 1,
        "the ICICLE backend must be loaded exactly once per process"
    );
    assert_eq!(
        end.ntt_domain_inits, 1,
        "the shared NTT domain must be initialised exactly once per process"
    );
    assert!(
        end.calibration_runs <= 1,
        "the lazy calibration must run at most once per process"
    );
    assert_eq!(end.gpu_fallbacks, 0, "a GPU op fell back to the CPU");
    for line in [0, 1] {
        assert!(end.msm(line) > c0.msm(line), "line {line} launched no MSM in the parallel run");
        assert!(end.ntt(line) > c0.ntt(line), "line {line} launched no NTT in the parallel run");
        assert!(end.msm(line) > 0 && end.ntt(line) > 0);
    }
}
