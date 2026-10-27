// This file is part of midnight-ledger.
// Copyright (C) Midnight Foundation
// SPDX-License-Identifier: Apache-2.0
//
// GPU proving benchmark on a REAL Ledger-9 (ledger-9.1.0.0-rc.6) Zswap+Dust
// transaction, shaped like the Parity (closed-CUDA) reference run. See
// midnight-bls12-381-cuda/openspec/changes/parity-comparable-benchmark.
//
// The transaction is the same shape as the reference fixture:
//   2 zswap spends + 2 zswap outputs + 1 dust spend = 5 proof jobs.
//
// Which prover runs: on Ledger 9 all five built-in circuits ship as legacy
// `ir-source[v2]` IR (=> IrMinorVersion::V0), so `zkir_v2::LocalProvingProvider`
// routes every job through the OLD pipeline: transient-crypto-old (2.2) ->
// midnight-zk-stdlib 1.3 -> midnight-proofs 0.7.x. GPU for this benchmark therefore
// means the 0.7.x fork (zkir feature `gpu-v1`). The test prints the observed key
// locations, IR versions and k values after proving, so this is checked per run.
//
// Seeds and timing follow the reference: the warmup proves seed BENCH_SEED-1
// (24301), timed iteration i proves seed BENCH_SEED+i (24302, 24303, ...). Every
// seed gets its own funded TestState (key generation, Dust, two shielded coins),
// built outside the timers. Two numbers are reported per iteration:
//   parity  construct the unbalanced tx from the funded state + balance_tx +
//           Transaction::prove + proof verification + seal + tagged serialization
//   prove   Transaction::prove alone (the five proof jobs)
// In `gpu` builds zkir self-verifies every V0 proof inside Transaction::prove (as
// Ledger 8 and the reference do), so verification is part of both numbers, and the
// output calls the second one (and each job's time) `prove+self-verify`. Builds
// without `gpu` keep upstream Ledger 9's unverified V0 path, so there the ledger's
// well-formedness check is timed in its place to keep the parity region the same
// shape; use one `gpu` build for both arms so they measure the same work. Every
// sealed tx also passes the ledger's well-formedness check after the timers (proofs
// against the built-in verifier keys, binding, signatures, limits); failure panics.
// That check is timed on its own as `wf-check`: it verifies the same five proofs, so
// it shows roughly how much of `prove+self-verify` is verification.
// The one check skipped is fee balancing: TestState::balance_tx pays the fee it
// estimates for the unproven, unsealed tx, which falls short of the sealed tx's fee
// (by about 0.24% here, mostly the cost of checking the seal's binding), so the
// harness reports each tx's Dust balance after its fee instead.
//
// Params and keys: the data provider keeps nothing in memory, so every job re-reads
// and SHA-256s its prover/verifier/IR files (as the proof server does); the prover
// key is then deserialized from transient-crypto-old's PK_CACHE once warm. By
// default SRS params are memoized per k, so the warmup really warms them: the 0.8.x
// params are read, hashed and parsed once per k, and `gpu` builds of zkir also reuse
// one converted 0.7.x instance per k, with its uploaded GPU bases. That measures a
// server that holds its params. This repo's proof server does not: it proves with
// the data provider's params, re-read for every job, so zkir also converts them (and
// in `gpu` builds uploads the bases) per job. BENCH_PARAMS_CACHE=0 measures that
// configuration by reloading the params for every job; report both, as the RESULT
// line names the setting.
//
// Byte-exactness (BENCH_DUMP / BENCH_EXPECT): everything this harness controls is
// seeded, yet proofs are not reproducible with upstream midnight-proofs 0.7.3: its
// lookup argument (`permute_expression_pair`, src/plonk/lookup/prover.rs:399 and
// :430 upstream, :455 and :485 in the GPU fork) fills the permuted table's repeated
// rows by iterating a std `HashMap`, whose order is random per map instance. Two
// proves of one seed therefore differ, on CPU as on GPU; with that map made a
// BTreeMap, this benchmark's proofs are byte-identical across processes. So a run
// with BENCH_DUMP or BENCH_EXPECT first proves the warmup seed twice to learn
// whether its prover is deterministic, and BENCH_EXPECT then requires:
//   - both runs deterministic: every sealed tx identical, byte for byte;
//   - otherwise: every sealed tx identical with its proofs erased, and the same
//     proofs (circuit, length) in the same order; each proof's first differing byte
//     is reported.
// BENCH_EXPECT also puts every transaction of the dumped run through the
// well-formedness check again, in this process. A GPU run cannot check its own proofs
// independently: under MIDNIGHT_DEVICE=gpu the verifier's MSMs run on the GPU too, so
// a GPU MSM that returned a wrong result without failing could make proofs that the
// same run's verification accepts. Running the GPU arm first and the CPU arm against
// its dump therefore checks every GPU proof with the CPU (BLST) verifier.
//
// Backend selection is via environment:
//   MIDNIGHT_DEVICE = cpu | gpu | (unset => Auto thresholds)
//     gpu and Auto need a `--features gpu` build: the test initializes the backend
//     before the warmup and panics unless the 0.7.x line came up. Auto also prints
//     its effective MSM / NTT size thresholds (env or persisted calibration).
//   ICICLE_BACKEND_INSTALL_DIR = dir with our compiled CUDA .so (gpu/Auto)
//   MIDNIGHT_GPU_STRICT = 1  => panic on any GPU->CPU failure fallback. Required for
//     gpu and Auto: without it a failed GPU op silently runs on CPU.
// Tunables:
//   BENCH_SEED         (default 24302)  first timed seed; the warmup uses BENCH_SEED-1
//   BENCH_REPEAT       (default 5)      timed iterations
//   BENCH_WARMUP       (default 1)      untimed warmup proves (seeds BENCH_SEED-1, -2, ...)
//   BENCH_PARAMS_CACHE (default 1)      0 => reload SRS params for every proof job, as
//                                       the proof server does (see Params and keys)
//   BENCH_DUMP         (optional path)  write every timed tx and its proofs here
//   BENCH_EXPECT       (optional path)  compare with another run's BENCH_DUMP and check
//                                       its transactions here; panics on mismatch or
//                                       rejection (see Byte-exactness)
//
// Key material: zswap/10/{spend,output}.{prover,verifier,bzkir}, dust/10/spend.* and
// bls_midnight_2p{k} are resolved by the data provider from $MIDNIGHT_PP (else
// $XDG_CACHE_HOME/midnight/zk-params, else ~/.cache/midnight/zk-params) and fetched
// from $MIDNIGHT_PARAM_SOURCE (default https://srs.midnight.network/) when missing.
//
// Run both arms from one `gpu` build (two processes: the backend config is frozen per
// process), the GPU arm first, then the CPU arm against the GPU dump, which verifies
// the GPU proofs on CPU (see Byte-exactness):
//   export MIDNIGHT_LEDGER_TEST_STATIC_DIR="" MIDNIGHT_GPU_STRICT=1 \
//     ICICLE_BACKEND_INSTALL_DIR=~/.local/icicle-backends/ours
//   MIDNIGHT_DEVICE=gpu BENCH_DUMP=/tmp/gpu.bin \
//     cargo test --release -p midnight-ledger-v9 --features proving,test-utilities,gpu \
//     --test gpu_benchmark -- --nocapture --exact real_tx_prove_bench
//   MIDNIGHT_DEVICE=cpu BENCH_EXPECT=/tmp/gpu.bin \
//     cargo test --release -p midnight-ledger-v9 --features proving,test-utilities,gpu \
//     --test gpu_benchmark -- --nocapture --exact real_tx_prove_bench
// (Running the test binary directly also needs
//  LD_LIBRARY_PATH=$PWD/target/release/deps/icicle/lib.)
//
#![cfg(all(feature = "proving", feature = "test-utilities"))]

use base_crypto::rng::SplittableRng;
use coin_structure::coin::{Info as CoinInfo, ShieldedTokenType};
use midnight_ledger::prove::Resolver;
use midnight_ledger::structure::{ProofMarker, Transaction};
use midnight_ledger::test_utilities::{TestState, test_resolver};
use midnight_ledger::verify::WellFormedStrictness;
use midnight_ledger_v9 as midnight_ledger;
use onchain_runtime::cost_model::INITIAL_COST_MODEL;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use storage::db::InMemoryDB;
use storage::storage::HashMap;
use transient_crypto::commitment::PureGeneratorPedersen;
use transient_crypto::proofs::{
    ParamsProver, ParamsProverProvider, ProofPreimage, ProvingProvider, Resolver as ResolverT,
};
use zkir_v2::{IrMinorVersion, IrSource, LocalProvingProvider};
use zswap::{Delta, Offer as ZswapOffer, Output as ZswapOutput};

use lazy_static::lazy_static;

lazy_static! {
    // test_name = "" -> the file-closure resolves contract keys under {DIR}//keys/...
    // (none needed here); the pure zswap+dust keys come from the data-provider cache.
    static ref RESOLVER: Resolver = test_resolver("");
    static ref PARAMS: MemoParams = MemoParams {
        enabled: env_u64("BENCH_PARAMS_CACHE", 1) != 0,
        by_k: Mutex::new(BTreeMap::new()),
    };
}

/// In `gpu` builds zkir verifies every V0 proof inside `Transaction::prove`.
const PROVER_SELF_VERIFIES: bool = cfg!(feature = "gpu");

/// What the `prove_ms` figures (and the per-job times) measure.
const PROVE: &str = if PROVER_SELF_VERIFIES {
    "prove+self-verify"
} else {
    "prove"
};

type SealedTx = Transaction<(), ProofMarker, PureGeneratorPedersen, InMemoryDB>;

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn ms_since(t0: Instant) -> f64 {
    t0.elapsed().as_secs_f64() * 1e3
}

/// `RESOLVER`'s SRS params, memoized per k (unless BENCH_PARAMS_CACHE=0).
struct MemoParams {
    enabled: bool,
    by_k: Mutex<BTreeMap<u8, ParamsProver>>,
}

impl ParamsProverProvider for MemoParams {
    async fn get_params(&self, k: u8) -> std::io::Result<ParamsProver> {
        if !self.enabled {
            return RESOLVER.get_params(k).await;
        }
        if let Some(params) = self.by_k.lock().expect("params cache poisoned").get(&k) {
            return Ok(params.clone());
        }
        let params = RESOLVER.get_params(k).await?;
        let mut by_k = self.by_k.lock().expect("params cache poisoned");
        Ok(by_k.entry(k).or_insert(params).clone())
    }
}

/// One proof job, as seen by [`Recording`].
struct Job {
    key: String,
    ms: f64,
    /// The serialized statement; sorting on it gives the jobs of a seed a stable
    /// order whatever order they finished in.
    statement: Vec<u8>,
    proof: Vec<u8>,
}

/// Transparent `ProvingProvider` wrapper that records, per proof job, the key
/// location, the wall time spent inside the inner provider's `prove` and the proof.
/// (Jobs are driven by `join_all` on one task and each proof is synchronous
/// CPU/GPU work, so per-job times are effectively sequential and sum to ~total.)
struct Recording<P> {
    inner: P,
    jobs: Arc<Mutex<Vec<Job>>>,
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
        let ms = ms_since(t0);
        if let Ok(proof) = &res {
            let mut statement = Vec::new();
            serialize::tagged_serialize(preimage, &mut statement).expect("serialize preimage");
            if let Some(binding_input) = overwrite_binding_input {
                statement.extend(binding_input.as_le_bytes());
            }
            self.jobs.lock().expect("job log poisoned").push(Job {
                key: preimage.key_location.0.to_string(),
                ms,
                statement,
                proof: proof.0.clone(),
            });
        }
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

/// Shielded value of each coin: two are seeded so balance_tx spends exactly 2 inputs.
const V: u128 = 5_000_000_000;

/// UNTIMED: the funded state for `seed` (keys, 1 night utxo generating Dust, two
/// spendable shielded coins of value V), and the rng to build the tx with.
async fn funded_state(seed: u64) -> (TestState<InMemoryDB>, StdRng) {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut state: TestState<InMemoryDB> = TestState::new(&mut rng);
    let token: ShieldedTokenType = Default::default();
    // Dust to pay fees (1 night utxo => 1 dust spend job after balancing).
    state.give_fee_token(&mut rng, 1).await;
    state.rewards_shielded(&mut rng, token, V);
    state.rewards_shielded(&mut rng, token, V);
    (state, rng)
}

/// The ledger's own acceptance check; panics unless `tx` is well-formed against
/// `state`: zswap and dust proofs verified with the built-in verifier keys, the
/// binding, signatures and limits. Everything but the fee balance, which
/// `TestState::balance_tx` sizes from its estimate for the unproven, unsealed tx;
/// the sealed tx costs a little more (see [`dust_fee_balance`]). `what` names the
/// tx in the panic message.
fn assert_well_formed(state: &TestState<InMemoryDB>, tx: &SealedTx, what: std::fmt::Arguments) {
    let mut strictness = WellFormedStrictness::default();
    strictness.enforce_balancing = false;
    if let Err(e) = tx.well_formed(&state.ledger, strictness, state.time) {
        panic!("{what}: the sealed transaction is not well-formed: {e}");
    }
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

/// The 0.7.x line's GPU backend, with the MSM / NTT size thresholds that route
/// each operation under Auto.
#[cfg(feature = "gpu")]
fn gpu_backend_info() -> String {
    use midnight_proofs_v1::gpu_accel::GpuAccelerator;
    midnight_proofs_v1::gpu_accel::global_accelerator().backend_info()
}

/// The sealed tx's Dust balance after its own fee: what it pays minus what it
/// costs, in specks (negative when underpaid), and the fee.
fn dust_fee_balance(state: &TestState<InMemoryDB>, tx: &SealedTx) -> (i128, u128) {
    let fee = tx
        .fees(&state.ledger.parameters, false)
        .expect("fee calculation");
    let balance = tx.balance(Some(fee)).expect("balance");
    (
        balance
            .get(&(midnight_ledger::structure::FEE_TOKEN, 0))
            .copied()
            .unwrap_or(0),
        fee,
    )
}

/// One proven transaction and its timings.
struct Proven {
    seed: u64,
    parity_ms: f64,
    prove_ms: f64,
    /// The ledger's well-formedness check of the sealed tx (part of `parity_ms` only
    /// in builds without `gpu`).
    check_ms: f64,
    /// The tagged serialization of the sealed tx.
    tx: Vec<u8>,
    /// The same with every proof erased.
    erased: Vec<u8>,
    /// Ordered by (circuit, statement).
    jobs: Vec<Job>,
    /// [`dust_fee_balance`] of the sealed tx.
    fee_balance: (i128, u128),
}

/// Builds, balances, proves, verifies, seals and serializes the 5-job tx for
/// `seed`, timing it as described in the header.
async fn prove_tx(seed: u64) -> Proven {
    let (mut state, mut rng) = funded_state(seed).await;
    let jobs = Arc::new(Mutex::new(Vec::new()));

    let t_parity = Instant::now();
    // A guaranteed (segment 0) shielded offer with 2 self-outputs of V each and a
    // delta of -2V. balance_tx then spends the 2 seeded coins (2 inputs, no change
    // output since they sum to exactly 2V) and adds 1 dust spend
    // => 2 spend + 2 output + 1 dust = 5 jobs.
    let token: ShieldedTokenType = Default::default();
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

    let provider = Recording {
        inner: LocalProvingProvider {
            rng: rng.split(),
            params: &*PARAMS,
            resolver: &*RESOLVER,
        },
        jobs: jobs.clone(),
    };
    let t_prove = Instant::now();
    let proven = balanced
        .prove(provider, &INITIAL_COST_MODEL)
        .await
        .expect("prove failed");
    let prove_ms = ms_since(t_prove);

    let sealed: SealedTx = proven.seal(rng.split());
    let check = || {
        let t_check = Instant::now();
        assert_well_formed(&state, &sealed, format_args!("seed {seed}"));
        ms_since(t_check)
    };
    let check_in_parity = (!PROVER_SELF_VERIFIES).then(check);
    let mut tx_bytes = Vec::new();
    serialize::tagged_serialize(&sealed, &mut tx_bytes).expect("serialize tx");
    let parity_ms = ms_since(t_parity);

    let check_ms = check_in_parity.unwrap_or_else(check);
    let fee_balance = dust_fee_balance(&state, &sealed);
    let mut erased = Vec::new();
    serialize::tagged_serialize(&sealed.erase_proofs(), &mut erased).expect("serialize tx");
    let mut jobs = std::mem::take(&mut *jobs.lock().expect("job log poisoned"));
    jobs.sort_by(|a, b| (&a.key, &a.statement).cmp(&(&b.key, &b.statement)));
    Proven {
        seed,
        parity_ms,
        prove_ms,
        check_ms,
        tx: tx_bytes,
        erased,
        jobs,
        fee_balance,
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

fn mean(xs: &[f64]) -> f64 {
    xs.iter().sum::<f64>() / xs.len().max(1) as f64
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

// ---- BENCH_DUMP / BENCH_EXPECT file: a magic, whether the writing run's prover was
// deterministic, then per timed iteration its seed, sealed tx, proof-erased tx and
// (circuit, proof) pairs. Integers are u64 LE; byte strings are length-prefixed. ----

const DUMP_MAGIC: &[u8] = b"midnight-ledger-v9 gpu_benchmark dump v1\n";

fn put_u64(out: &mut Vec<u8>, x: u64) {
    out.extend(x.to_le_bytes());
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    put_u64(out, bytes.len() as u64);
    out.extend(bytes);
}

fn take<'a>(input: &mut &'a [u8], n: usize) -> &'a [u8] {
    assert!(input.len() >= n, "BENCH_EXPECT: truncated dump file");
    let (head, rest) = input.split_at(n);
    *input = rest;
    head
}

fn take_u64(input: &mut &[u8]) -> u64 {
    u64::from_le_bytes(take(input, 8).try_into().expect("8 bytes"))
}

fn take_bytes(input: &mut &[u8]) -> Vec<u8> {
    let n = take_u64(input) as usize;
    take(input, n).to_vec()
}

/// One timed iteration as recorded in a dump.
struct Recorded {
    seed: u64,
    tx: Vec<u8>,
    erased: Vec<u8>,
    proofs: Vec<(String, Vec<u8>)>,
}

fn write_dump(path: &str, deterministic: bool, runs: &[Proven]) {
    let mut out = DUMP_MAGIC.to_vec();
    put_u64(&mut out, deterministic as u64);
    put_u64(&mut out, runs.len() as u64);
    for run in runs {
        put_u64(&mut out, run.seed);
        put_bytes(&mut out, &run.tx);
        put_bytes(&mut out, &run.erased);
        put_u64(&mut out, run.jobs.len() as u64);
        for job in &run.jobs {
            put_bytes(&mut out, job.key.as_bytes());
            put_bytes(&mut out, &job.proof);
        }
    }
    std::fs::write(path, &out).expect("write BENCH_DUMP");
    eprintln!(
        "   wrote {} timed transactions ({} bytes) -> {path}",
        runs.len(),
        out.len()
    );
}

fn read_dump(path: &str) -> (bool, Vec<Recorded>) {
    let data = std::fs::read(path).expect("read BENCH_EXPECT");
    let mut input = &data[..];
    assert!(
        take(&mut input, DUMP_MAGIC.len()) == DUMP_MAGIC,
        "BENCH_EXPECT {path} is not a gpu_benchmark dump"
    );
    let deterministic = take_u64(&mut input) != 0;
    let n = take_u64(&mut input);
    let runs = (0..n)
        .map(|_| {
            let seed = take_u64(&mut input);
            let tx = take_bytes(&mut input);
            let erased = take_bytes(&mut input);
            let proofs = (0..take_u64(&mut input))
                .map(|_| {
                    let key = String::from_utf8(take_bytes(&mut input)).expect("utf-8 key");
                    (key, take_bytes(&mut input))
                })
                .collect();
            Recorded {
                seed,
                tx,
                erased,
                proofs,
            }
        })
        .collect();
    assert!(input.is_empty(), "BENCH_EXPECT {path}: trailing bytes");
    (deterministic, runs)
}

fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
    a.iter().zip(b).position(|(x, y)| x != y)
}

/// Compares this run against another run's dump; panics on any mismatch the two
/// runs' provers do not explain (see the header), or unless every dumped tx is
/// well-formed here. `cpu_verifier`: this process verifies with the CPU (BLST).
async fn check_expected(path: &str, deterministic: bool, runs: &[Proven], cpu_verifier: bool) {
    let (expected_deterministic, expected) = read_dump(path);
    let exact_required = deterministic && expected_deterministic;
    assert_eq!(
        expected.len(),
        runs.len(),
        "BENCH_EXPECT {path}: it holds {} timed iterations, this run made {}",
        expected.len(),
        runs.len()
    );
    let mut exact = 0;
    for (want, got) in expected.iter().zip(runs) {
        let seed = got.seed;
        assert_eq!(want.seed, seed, "BENCH_EXPECT {path}: seed mismatch");
        // The funded state is a function of the seed, so the dumped tx is checked
        // against the state it was built on.
        let (state, _) = funded_state(seed).await;
        let tx: SealedTx = serialize::tagged_deserialize(&mut &want.tx[..])
            .unwrap_or_else(|e| panic!("BENCH_EXPECT {path}: seed {seed}: bad tx: {e}"));
        assert_well_formed(&state, &tx, format_args!("seed {seed} of {path}"));
        if want.tx == got.tx {
            exact += 1;
            eprintln!(
                "  [expect] seed {seed}: BYTE-EXACT ({} bytes)",
                got.tx.len()
            );
            continue;
        }
        assert!(
            !exact_required,
            "seed {seed}: the sealed tx differs from {path} (first difference at byte {:?}) \
             although both provers are deterministic",
            first_difference(&want.tx, &got.tx)
        );
        assert!(
            want.erased == got.erased,
            "seed {seed}: the tx with proofs erased differs from {path} (first difference at \
             byte {:?}): the two runs did not build, balance and seal the same transaction",
            first_difference(&want.erased, &got.erased)
        );
        let want_shape: Vec<_> = want.proofs.iter().map(|(k, p)| (k, p.len())).collect();
        let got_shape: Vec<_> = got.jobs.iter().map(|j| (&j.key, j.proof.len())).collect();
        assert_eq!(
            want_shape, got_shape,
            "seed {seed}: the proofs (circuit, length) differ from {path}"
        );
        let diverge = want
            .proofs
            .iter()
            .zip(&got.jobs)
            .map(
                |((key, proof), job)| match first_difference(proof, &job.proof) {
                    Some(at) => format!("{key}@{at}/{}", proof.len()),
                    None => format!("{key}=equal"),
                },
            )
            .collect::<Vec<_>>();
        eprintln!(
            "  [expect] seed {seed}: identical with proofs erased; proof bytes first differ at \
             {}",
            diverge.join(" ")
        );
    }
    eprintln!(
        "== EXPECT {path}: {exact}/{} BYTE-EXACT; deterministic prover: this run \
         {deterministic}, expected run {expected_deterministic} ==",
        runs.len()
    );
    if exact < runs.len() {
        eprintln!(
            "   the others are identical except for proof bytes, which a non-deterministic \
             prover does not reproduce"
        );
    }
    if cpu_verifier {
        eprintln!(
            "   all {} dumped transactions are well-formed with the CPU (BLST) verifier",
            expected.len()
        );
    } else {
        eprintln!(
            "   all {} dumped transactions are well-formed here, but this process verifies \
             on the GPU: run the CPU arm against a GPU dump for an independent check",
            expected.len()
        );
    }
}

#[tokio::test]
async fn real_tx_prove_bench() {
    let seed = env_u64("BENCH_SEED", 24302);
    let repeat = env_u64("BENCH_REPEAT", 5) as usize;
    let warmup = env_u64("BENCH_WARMUP", 1);
    let device = std::env::var("MIDNIGHT_DEVICE").unwrap_or_else(|_| "auto".into());
    let strict = std::env::var("MIDNIGHT_GPU_STRICT").unwrap_or_else(|_| "0".into());
    let dump = std::env::var("BENCH_DUMP").ok();
    let expect = std::env::var("BENCH_EXPECT").ok();

    eprintln!(
        "== real_tx_prove_bench :: device={device} strict={strict} seeds={seed}..={} \
         warmup={warmup} params_cache={} prover_self_verifies={PROVER_SELF_VERIFIES} ==",
        seed + repeat as u64 - 1,
        PARAMS.enabled,
    );
    assert!(
        cfg!(feature = "proof-verifying"),
        "the well-formedness check needs the default `proof-verifying` feature"
    );

    // A GPU (or Auto) arm must actually be able to use the GPU: without the feature no
    // backend is linked and every op silently runs on CPU, even under strict mode.
    if !cpu_device(&device) {
        assert!(
            cfg!(feature = "gpu"),
            "MIDNIGHT_DEVICE={device} needs a GPU build: add --features gpu \
             (or set MIDNIGHT_DEVICE=cpu for the CPU arm)"
        );
        // Without strict mode a GPU op that fails is redone on CPU with only a
        // [GPU-FALLBACK] line on stderr, and the arm would still report GPU numbers.
        assert!(
            strict_mode(&strict),
            "MIDNIGHT_DEVICE={device} needs MIDNIGHT_GPU_STRICT=1, so that a GPU operation \
             that fails panics instead of silently running on CPU"
        );
        let status = zkir_v2::init_gpu_backend();
        eprintln!("  [gpu] init: {status:?}");
        assert!(
            matches!(status.v1, zkir_v2::GpuLineStatus::Initialized(_)),
            "the midnight-proofs 0.7.x GPU backend did not initialize ({:?}); see the \
             [GPU-FALLBACK] / [GPU-WARN] line above",
            status.v1
        );
        // Auto keeps operations below these sizes on CPU (by design, not a fallback).
        if !gpu_device(&device) {
            #[cfg(feature = "gpu")]
            eprintln!("  [gpu] Auto: {}", gpu_backend_info());
        }
    }

    // ---- warmup (UNTIMED): params, converted params + GPU bases, PK_CACHE, verifier ----
    let mut first_warmup = None;
    for j in 0..warmup {
        let run = prove_tx(seed - 1 - j).await;
        eprintln!(
            "  warmup seed={}: parity {:.1} ms, {PROVE} {:.1} ms",
            run.seed, run.parity_ms, run.prove_ms
        );
        first_warmup.get_or_insert(run);
    }
    // Byte comparisons need to know whether this prover is deterministic.
    let deterministic = if dump.is_some() || expect.is_some() {
        let first = match first_warmup {
            Some(run) => run,
            None => prove_tx(seed - 1).await,
        };
        let again = prove_tx(seed - 1).await;
        let same = first.tx == again.tx;
        if same {
            eprintln!(
                "  [determinism] seed {} proved twice: BYTE-IDENTICAL",
                first.seed
            );
        } else {
            eprintln!(
                "  [determinism] seed {} proved twice: DIFFERS (first at byte {:?}); this \
                 prover does not reproduce proof bytes",
                first.seed,
                first_difference(&first.tx, &again.tx)
            );
        }
        same
    } else {
        false
    };

    // ---- timed iterations ----
    let mut runs = Vec::with_capacity(repeat);
    for i in 0..repeat {
        let run = prove_tx(seed + i as u64).await;
        eprintln!(
            "  iter {i} seed={}: parity {:.1} ms, {PROVE} {:.1} ms ({} bytes)",
            run.seed,
            run.parity_ms,
            run.prove_ms,
            run.tx.len()
        );
        runs.push(run);
    }

    let parity: Vec<f64> = runs.iter().map(|r| r.parity_ms).collect();
    let prove: Vec<f64> = runs.iter().map(|r| r.prove_ms).collect();
    let check: Vec<f64> = runs.iter().map(|r| r.check_ms).collect();
    let mut per_job: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for job in runs.iter().flat_map(|r| &r.jobs) {
        per_job.entry(job.key.clone()).or_default().push(job.ms);
    }
    eprintln!(
        "== RESULT device={device} params_cache={} parity median={:.1}ms mean={:.1}ms | {PROVE} \
         median={:.1}ms mean={:.1}ms | wf-check median={:.1}ms | bytes={:?} jobs/tx={} \
         seeds={seed}..={} ==",
        PARAMS.enabled,
        median(parity.clone()),
        mean(&parity),
        median(prove.clone()),
        mean(&prove),
        median(check),
        runs.iter().map(|r| r.tx.len()).collect::<Vec<_>>(),
        runs.first().map_or(0, |r| r.jobs.len()),
        seed + repeat as u64 - 1,
    );
    for (key, ms) in &per_job {
        eprintln!(
            "  [job] {key:<24} count={:<3} per-job {PROVE} median={:.1}ms",
            ms.len(),
            median(ms.clone())
        );
    }
    report_circuits(per_job.keys().cloned()).await;
    for run in &runs {
        let (balance, fee) = run.fee_balance;
        eprintln!(
            "  [fee] seed {}: Dust balance after its {fee}-speck fee: {balance} ({:+.3}%)",
            run.seed,
            balance as f64 * 100.0 / fee as f64
        );
    }

    if let Some(path) = &dump {
        write_dump(path, deterministic, &runs);
    }
    if let Some(path) = &expect {
        // Without `gpu` every op runs on the CPU, whatever MIDNIGHT_DEVICE says.
        let cpu_verifier = !cfg!(feature = "gpu") || cpu_device(&device);
        check_expected(path, deterministic, &runs, cpu_verifier).await;
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
