// This file is part of midnight-ledger.
// Copyright (C) Midnight Foundation
// SPDX-License-Identifier: Apache-2.0
//
// Parity-comparable GPU proving benchmark on a REAL Ledger-9 (ledger-9.1.0.0-rc.6)
// Zswap+Dust transaction. See midnight-bls12-381-cuda/openspec/changes/parity-comparable-benchmark.
//
// The transaction is the same shape as the reference (Parity closed-CUDA) fixture:
//   2 zswap spends + 2 zswap outputs + 1 dust spend = 5 proof jobs.
//
// Which prover runs: on Ledger 9 all five built-in circuits ship as legacy
// `ir-source[v2]` IR (=> IrMinorVersion::V0), so `zkir_v2::LocalProvingProvider`
// routes every job through the OLD pipeline: transient-crypto-old (2.2) ->
// midnight-zk-stdlib 1.3 -> midnight-proofs 0.7.x. GPU for this benchmark therefore
// means the 0.7.x fork (zkir feature `gpu-v1`). The test prints the observed key
// locations, IR versions and k values after proving, so this is checked per run.
//
// Backend selection is entirely via environment (the timed region only proves; the
// backend is whatever the process is configured for):
//   MIDNIGHT_DEVICE = cpu | gpu | (unset => Auto thresholds)
//   ICICLE_BACKEND_INSTALL_DIR = dir with our compiled CUDA .so (for the gpu arm)
//   MIDNIGHT_GPU_STRICT = 1  => the CUDA arm PANICS on any silent CPU fallback,
//                               so a green run under strict IS the no-fallback proof.
// Tunables:
//   BENCH_SEED   (default 24302)  the deterministic fixture+prove seed
//   BENCH_REPEAT (default 5)      timed iterations
//   BENCH_WARMUP (default 1)      untimed warmups (warm SRS params + PK_CACHE)
//   BENCH_DUMP   (optional path)  write the serialized proof bytes here (byte-exact diff)
//
// Key material: zswap/10/{spend,output}.{prover,verifier,bzkir}, dust/10/spend.* and
// bls_midnight_2p{k} are resolved by the data provider from $MIDNIGHT_PP (else
// $XDG_CACHE_HOME/midnight/zk-params, else ~/.cache/midnight/zk-params) and fetched
// from $MIDNIGHT_PARAM_SOURCE (default https://srs.midnight.network/) when missing.
//
// Run one arm:
//   MIDNIGHT_LEDGER_TEST_STATIC_DIR="" MIDNIGHT_DEVICE=cpu \
//     cargo test --release -p midnight-ledger-v9 --features proving,test-utilities \
//     --test gpu_benchmark -- --nocapture --exact real_tx_prove_bench
//
#![cfg(all(feature = "proving", feature = "test-utilities"))]

use base_crypto::rng::SplittableRng;
use coin_structure::coin::{Info as CoinInfo, ShieldedTokenType};
use midnight_ledger::prove::Resolver;
use midnight_ledger::structure::Transaction;
use midnight_ledger::test_utilities::{TestState, test_resolver};
use midnight_ledger_v9 as midnight_ledger;
use onchain_runtime::cost_model::INITIAL_COST_MODEL;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use storage::db::InMemoryDB;
use storage::storage::HashMap;
use transient_crypto::proofs::{ProofPreimage, ProvingProvider, Resolver as ResolverT};
use zkir_v2::{IrMinorVersion, IrSource, LocalProvingProvider};
use zswap::{Delta, Offer as ZswapOffer, Output as ZswapOutput};

use lazy_static::lazy_static;

lazy_static! {
    // test_name = "" -> the file-closure resolves contract keys under {DIR}//keys/...
    // (none needed here); the pure zswap+dust keys come from the data-provider cache.
    static ref RESOLVER: Resolver = test_resolver("");
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Transparent `ProvingProvider` wrapper that records, per proof job, the key
/// location and the wall time spent inside the inner provider's `prove`.
/// (Jobs are driven by `join_all` on one task and each proof is synchronous
/// CPU/GPU work, so per-job times are effectively sequential and sum to ~total.)
struct Recording<P> {
    inner: P,
    jobs: Arc<Mutex<Vec<(String, f64)>>>,
}

impl<P: ProvingProvider> ProvingProvider for Recording<P> {
    async fn check(&self, preimage: &ProofPreimage) -> Result<Vec<Option<usize>>, anyhow::Error> {
        self.inner.check(preimage).await
    }

    async fn prove(
        self,
        preimage: &ProofPreimage,
        overwrite_binding_input: Option<transient_crypto::curve::Fr>,
    ) -> Result<transient_crypto::proofs::Proof, anyhow::Error> {
        let t0 = Instant::now();
        let res = self.inner.prove(preimage, overwrite_binding_input).await;
        let ms = t0.elapsed().as_secs_f64() * 1e3;
        self.jobs
            .lock()
            .expect("job log poisoned")
            .push((preimage.key_location.0.to_string(), ms));
        res
    }

    fn split(&mut self) -> Self {
        Recording {
            inner: self.inner.split(),
            jobs: self.jobs.clone(),
        }
    }

    fn resolver(&self) -> &impl ResolverT {
        self.inner.resolver()
    }
}

/// Build the real 5-job transaction, UNTIMED, fully deterministic from `seed`.
/// Returns the balanced-but-unproven tx plus the rng to prove it with, so the
/// proving randomness is identical across iterations and across CPU/GPU arms.
async fn build_fixture(
    seed: u64,
) -> (
    Transaction<
        (),
        midnight_ledger::structure::ProofPreimageMarker,
        transient_crypto::commitment::PedersenRandomness,
        InMemoryDB,
    >,
    StdRng,
) {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut state: TestState<InMemoryDB> = TestState::new(&mut rng);

    // Shielded value V, seeded as two coins so balance_tx spends exactly 2 inputs.
    const V: u128 = 5_000_000_000;
    let token: ShieldedTokenType = Default::default();

    // Dust to pay fees (1 night utxo => 1 dust spend job after balancing).
    state.give_fee_token(&mut rng, 1).await;
    // Two spendable shielded coins of value V.
    state.rewards_shielded(&mut rng, token, V);
    state.rewards_shielded(&mut rng, token, V);

    // A guaranteed (segment 0) shielded offer with 2 self-outputs of V each and a
    // delta of -2V. balance_tx then spends the 2 seeded coins (2 inputs, no change
    // output since they sum to exactly 2V) and adds 1 dust spend
    // => 2 spend + 2 output + 1 dust = 5 jobs.
    let mk_output = |rng: &mut StdRng| {
        let coin = CoinInfo {
            nonce: rng.r#gen(),
            value: V,
            type_: token,
        };
        ZswapOutput::<_, InMemoryDB>::new(
            rng,
            &coin,
            Some(0u16),
            &state.zswap_keys.coin_public_key(),
            Some(state.zswap_keys.enc_public_key()),
        )
        .expect("output creation must succeed")
    };
    let out0 = mk_output(&mut rng);
    let out1 = mk_output(&mut rng);

    let offer = ZswapOffer {
        inputs: vec![].into(),
        outputs: vec![out0, out1].into(),
        transient: vec![].into(),
        deltas: vec![Delta {
            token_type: token,
            value: -((2u128 * V) as i128),
        }]
        .into(),
    };

    let tx = Transaction::<(), _, _, InMemoryDB>::new(
        "local-test",
        HashMap::new(),
        Some(offer),
        HashMap::new(),
    );

    let balanced = state
        .balance_tx(rng.split(), tx, &RESOLVER)
        .await
        .expect("balance_tx must succeed");

    (balanced, rng)
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

/// UNTIMED: for each key location that was proven, resolve the exact artifacts the
/// prover used and report IR tag, IR minor version, k, and which zk stack proves it.
async fn report_circuits(keys: impl IntoIterator<Item = String>) {
    use transient_crypto::proofs::{KeyLocation, Zkir};
    for key in keys {
        let material = RESOLVER
            .resolve_key(KeyLocation(key.clone().into()))
            .await
            .expect("resolve_key")
            .expect("key material present");
        let tag =
            serialize::peek_tag(&mut std::io::Cursor::new(&material.ir_source)).expect("ir tag");
        let ir =
            IrSource::load_from_tagged(std::io::Cursor::new(&material.ir_source)).expect("load ir");
        let pipeline = match ir.version {
            IrMinorVersion::V0 | IrMinorVersion::V1 => {
                "v1: transient-crypto-old -> zk-stdlib 1.x -> midnight-proofs 0.7.x"
            }
            _ => "v2: transient-crypto 3 -> zk-stdlib 2.x -> midnight-proofs 0.8.x",
        };
        eprintln!(
            "  [circuit] {key:<24} ir_tag={tag:<22} version={:?} k={} => {pipeline}",
            ir.version,
            ir.k()
        );
    }
}

#[tokio::test]
async fn real_tx_prove_bench() {
    let seed = env_u64("BENCH_SEED", 24302);
    let repeat = env_u64("BENCH_REPEAT", 5) as usize;
    let warmup = env_u64("BENCH_WARMUP", 1) as usize;
    let device = std::env::var("MIDNIGHT_DEVICE").unwrap_or_else(|_| "auto".into());
    let strict = std::env::var("MIDNIGHT_GPU_STRICT").unwrap_or_else(|_| "0".into());

    eprintln!(
        "== real_tx_prove_bench :: device={device} strict={strict} seed={seed} \
         repeat={repeat} warmup={warmup} =="
    );

    // ---- warmup (UNTIMED): warms SRS params I/O + SHA256 + parse, and PK_CACHE ----
    for _ in 0..warmup {
        let (balanced, mut prng) = build_fixture(seed).await;
        let provider = LocalProvingProvider {
            rng: prng.split(),
            params: &*RESOLVER,
            resolver: &*RESOLVER,
        };
        let _ = balanced
            .prove(provider, &INITIAL_COST_MODEL)
            .await
            .expect("warmup prove failed");
    }

    // ---- timed iterations ----
    let mut times_ms: Vec<f64> = Vec::with_capacity(repeat);
    let mut proof_bytes: Vec<u8> = Vec::new();
    let mut sizes: Vec<usize> = Vec::new();
    let mut per_job: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut jobs_per_tx = 0usize;

    for i in 0..repeat {
        let (balanced, mut prng) = build_fixture(seed).await; // UNTIMED rebuild
        let jobs = Arc::new(Mutex::new(Vec::new()));
        let provider = Recording {
            inner: LocalProvingProvider {
                rng: prng.split(),
                params: &*RESOLVER,
                resolver: &*RESOLVER,
            },
            jobs: jobs.clone(),
        };

        let t0 = Instant::now();
        let proven = balanced
            .prove(provider, &INITIAL_COST_MODEL)
            .await
            .expect("prove failed");
        let dt: Duration = t0.elapsed();
        times_ms.push(dt.as_secs_f64() * 1000.0);

        let jobs = jobs.lock().expect("job log poisoned").clone();
        jobs_per_tx = jobs.len();
        for (key, ms) in jobs {
            per_job.entry(key).or_default().push(ms);
        }

        // serialize the proven tx for size + byte-exact comparison
        let mut buf = Vec::new();
        serialize::tagged_serialize(&proven, &mut buf).expect("serialize proof");
        sizes.push(buf.len());
        if i == 0 {
            proof_bytes = buf;
        } else {
            let same = buf == proof_bytes;
            eprintln!(
                "  [determinism] iter {i} vs iter 0: {} (len {} vs {})",
                if same { "BYTE-IDENTICAL" } else { "DIFFERS" },
                buf.len(),
                proof_bytes.len()
            );
        }
        eprintln!("  iter {i}: {:.1} ms  ({} bytes)", times_ms[i], sizes[i]);
    }

    let med = median(times_ms.clone());
    let mean = times_ms.iter().sum::<f64>() / times_ms.len().max(1) as f64;
    eprintln!(
        "== RESULT device={device} median={med:.1}ms mean={mean:.1}ms bytes={} jobs/tx={jobs_per_tx} seed={seed} ==",
        proof_bytes.len()
    );
    for (key, ms) in &per_job {
        eprintln!(
            "  [job] {key:<24} count={:<3} per-job median={:.1}ms",
            ms.len(),
            median(ms.clone())
        );
    }
    report_circuits(per_job.keys().cloned()).await;

    // Optional: dump the proof bytes so a runner can diff CPU vs CUDA byte-for-byte.
    if let Ok(path) = std::env::var("BENCH_DUMP") {
        std::fs::write(&path, &proof_bytes).expect("write BENCH_DUMP");
        eprintln!("   wrote {} proof bytes -> {path}", proof_bytes.len());
    }
}

/// Offline inventory of the built-in circuits shipped in-tree (no keys, no network):
/// IR tag, IR minor version, k, and which zk stack `LocalProvingProvider` routes
/// each one to. These are byte-identical to the data provider's
/// `{zswap,dust}/<static version>/*.bzkir` (same SHA-256 as the `.sha256` pins).
#[test]
fn builtin_circuit_inventory() {
    use transient_crypto::proofs::Zkir;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let circuits = [
        (
            "midnight/zswap/spend",
            root.join("../zswap/static/spend.bzkir"),
        ),
        (
            "midnight/zswap/output",
            root.join("../zswap/static/output.bzkir"),
        ),
        (
            "midnight/zswap/sign",
            root.join("../zswap/static/sign.bzkir"),
        ),
        ("midnight/dust/spend", root.join("static/dust/spend.bzkir")),
    ];
    for (key, path) in circuits {
        let bytes = std::fs::read(&path).expect("read in-tree bzkir");
        let tag = serialize::peek_tag(&mut std::io::Cursor::new(&bytes)).expect("ir tag");
        let ir = IrSource::load_from_tagged(std::io::Cursor::new(&bytes)).expect("load ir");
        let legacy = matches!(ir.version, IrMinorVersion::V0 | IrMinorVersion::V1);
        eprintln!(
            "  [inventory] {key:<24} ir_tag={tag:<22} version={:?} k={} => {}",
            ir.version,
            ir.k(),
            if legacy {
                "v1 pipeline (midnight-proofs 0.7.x)"
            } else {
                "v2 pipeline (midnight-proofs 0.8.x)"
            }
        );
        // Ledger 9 ships every built-in circuit as legacy IR; if this ever changes,
        // the GPU benchmark needs the 0.8.x fork too (zkir feature `gpu-v2`).
        assert!(legacy, "{key} is no longer on the legacy (0.7.x) pipeline");
    }
}
