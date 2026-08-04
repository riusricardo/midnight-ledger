// This file is part of midnight-ledger.
// Copyright (C) Midnight Foundation
// SPDX-License-Identifier: Apache-2.0
//
// Parity-comparable GPU proving benchmark on a REAL Ledger-8.2.0-rc.1 Zswap+Dust
// transaction. See midnight-bls12-381-cuda/openspec/changes/parity-comparable-benchmark.
//
// The transaction is the same shape as the reference (Parity closed-CUDA) fixture:
//   2 zswap spends + 2 zswap outputs + 1 dust spend = 5 proof jobs, max circuit k=15.
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
// Run one arm:
//   MIDNIGHT_LEDGER_TEST_STATIC_DIR="" MIDNIGHT_DEVICE=cpu \
//     cargo test -p midnight-ledger --features proving,test-utilities \
//     --test gpu_benchmark -- --nocapture --exact real_tx_prove_bench
//
#![cfg(all(feature = "proving", feature = "test-utilities"))]

use base_crypto::rng::SplittableRng;
use coin_structure::coin::{Info as CoinInfo, ShieldedTokenType};
use midnight_ledger::prove::Resolver;
use midnight_ledger::structure::Transaction;
use midnight_ledger::test_utilities::{TestState, test_resolver};
use onchain_runtime::cost_model::INITIAL_COST_MODEL;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::time::{Duration, Instant};
use storage::db::InMemoryDB;
use storage::storage::HashMap;
use zkir_v2::LocalProvingProvider;
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
    // delta of -2V. balance_tx then spends the 2 seeded coins (2 inputs) to cover
    // it and adds 1 dust spend => 2 spend + 2 output + 1 dust = 5 jobs.
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

    for i in 0..repeat {
        let (balanced, mut prng) = build_fixture(seed).await; // UNTIMED rebuild
        let provider = LocalProvingProvider {
            rng: prng.split(),
            params: &*RESOLVER,
            resolver: &*RESOLVER,
        };

        let t0 = Instant::now();
        let proven = balanced
            .prove(provider, &INITIAL_COST_MODEL)
            .await
            .expect("prove failed");
        let dt: Duration = t0.elapsed();
        times_ms.push(dt.as_secs_f64() * 1000.0);

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
    let mean = times_ms.iter().sum::<f64>() / times_ms.len() as f64;
    eprintln!(
        "== RESULT device={device} median={med:.1}ms mean={mean:.1}ms bytes={} seed={seed} ==",
        proof_bytes.len()
    );

    // Optional: dump the proof bytes so a runner can diff CPU vs CUDA byte-for-byte.
    if let Ok(path) = std::env::var("BENCH_DUMP") {
        std::fs::write(&path, &proof_bytes).expect("write BENCH_DUMP");
        eprintln!("   wrote {} proof bytes -> {path}", proof_bytes.len());
    }
}
