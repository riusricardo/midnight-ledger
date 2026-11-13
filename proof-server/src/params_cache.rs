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

//! Prover parameters kept in memory between proofs (GPU builds).
//!
//! The data provider reads, checks and parses a parameter file on every
//! `get_params` call, so every proof job got a new `ParamsKZG` instance. GPU
//! builds keep per instance what they derive from the parameters (zkir's
//! converted instance for IR V0/V1, the SRS bases uploaded to the device), so a
//! new instance per job redid that work for every proof. This cache keeps the
//! instances of the most recently used `k` while their bases fit a byte budget
//! (`--params-cache-mib`): a parameter set larger than the budget is still read
//! for every job, and a budget of zero turns the cache off. Evicting an instance
//! releases it (and what was derived from it) once the proofs using it finish.

use std::io;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use transient_crypto::proofs::{ParamsProver, ParamsProverProvider};

use crate::endpoints::PUBLIC_PARAMS;

/// The budget, in MiB, when nothing sets one (library users, tests).
pub const DEFAULT_PARAMS_CACHE_MIB: u64 = 256;

static BUDGET_BYTES: OnceLock<u64> = OnceLock::new();

/// Sets the cache's budget in MiB; only the first call counts (the binary makes
/// it at start-up, before serving).
pub fn set_budget_mib(mib: u64) {
    // Ignoring the result keeps the first setting, as documented.
    let _ = BUDGET_BYTES.set(mib.saturating_mul(1 << 20));
}

fn budget_bytes() -> u64 {
    *BUDGET_BYTES.get_or_init(|| DEFAULT_PARAMS_CACHE_MIB << 20)
}

/// [`PUBLIC_PARAMS`] behind the cache: the proving endpoints' parameters in GPU
/// builds.
pub(crate) static PROVER_PARAMS: ParamsCache = ParamsCache {
    entries: Mutex::new(Lru::new()),
};

/// Recently used prover parameters, by `k`, within the byte budget.
pub(crate) struct ParamsCache {
    entries: Mutex<Lru<ParamsProver>>,
}

impl ParamsCache {
    fn entries(&self) -> MutexGuard<'_, Lru<ParamsProver>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl ParamsProverProvider for ParamsCache {
    async fn get_params(&self, k: u8) -> io::Result<ParamsProver> {
        if let Some(params) = self.entries().get(k) {
            return Ok(params);
        }
        // Loaded without the lock held. Two workers missing the same `k` at once
        // both load it, and both go on with the instance recorded first.
        let params = PUBLIC_PARAMS.get_params(k).await?;
        let bytes = params_bytes(&params);
        Ok(self.entries().insert(k, params, bytes, budget_bytes()))
    }
}

/// The host memory of a parameter set's bases: the coefficient and the Lagrange
/// bases, of equal length.
fn params_bytes(params: &ParamsProver) -> u64 {
    2 * std::mem::size_of_val(params.as_ref().g_lagrange()) as u64
}

/// Entries by `k`, least recently used first, within a byte budget.
struct Lru<V> {
    entries: Vec<(u8, V, u64)>,
    used: u64,
}

impl<V: Clone> Lru<V> {
    const fn new() -> Self {
        Self {
            entries: Vec::new(),
            used: 0,
        }
    }

    /// The value recorded for `k`, which becomes the most recently used.
    fn get(&mut self, k: u8) -> Option<V> {
        let position = self.entries.iter().position(|(key, ..)| *key == k)?;
        let entry = self.entries.remove(position);
        let value = entry.1.clone();
        self.entries.push(entry);
        Some(value)
    }

    /// Records `value`, `bytes` large, for `k`, and returns the value to use: the
    /// one already recorded for `k`, if another caller recorded one first, so that
    /// every caller shares one instance; otherwise `value`, recorded unless it alone
    /// exceeds `budget`, after evicting the least recently used entries to make room.
    fn insert(&mut self, k: u8, value: V, bytes: u64, budget: u64) -> V {
        if let Some(recorded) = self.get(k) {
            return recorded;
        }
        if bytes > budget {
            return value;
        }
        // Terminates: while `used` exceeds `budget - bytes`, some entry is left.
        while self.used.saturating_add(bytes) > budget {
            let (_, _, evicted) = self.entries.remove(0);
            self.used -= evicted;
        }
        self.used += bytes;
        self.entries.push((k, value.clone(), bytes));
        value
    }
}

#[cfg(test)]
mod tests {
    use super::Lru;
    use std::sync::Arc;

    fn keys(lru: &Lru<Arc<u8>>) -> Vec<u8> {
        lru.entries.iter().map(|(k, ..)| *k).collect()
    }

    #[test]
    fn a_recorded_value_is_shared() {
        let mut lru = Lru::new();
        let first = Arc::new(13);
        let used = lru.insert(13, first.clone(), 10, 100);
        assert!(Arc::ptr_eq(&used, &first));
        assert!(Arc::ptr_eq(&lru.get(13).expect("recorded"), &first));
        // A later load of the same k goes on with the instance recorded first.
        let second = Arc::new(13);
        assert!(Arc::ptr_eq(&lru.insert(13, second, 10, 100), &first));
        assert_eq!(lru.used, 10);
    }

    #[test]
    fn the_least_recently_used_are_evicted_to_fit() {
        let mut lru = Lru::new();
        for k in [13, 14, 15] {
            lru.insert(k, Arc::new(k), 30, 100);
        }
        // Touching 13 makes 14 the least recently used.
        lru.get(13);
        lru.insert(16, Arc::new(16), 30, 100);
        assert_eq!(keys(&lru), [15, 13, 16]);
        lru.insert(17, Arc::new(17), 70, 100);
        assert_eq!(keys(&lru), [16, 17]);
        assert_eq!(lru.used, 100);
        assert!(lru.get(14).is_none());
    }

    #[test]
    fn a_value_larger_than_the_budget_is_used_uncached() {
        let mut lru = Lru::new();
        lru.insert(13, Arc::new(13), 30, 100);
        let large = Arc::new(24);
        assert!(Arc::ptr_eq(
            &lru.insert(24, large.clone(), 101, 100),
            &large
        ));
        assert!(lru.get(24).is_none());
        // Nothing was evicted for it.
        assert_eq!(keys(&lru), [13]);
    }

    #[test]
    fn a_zero_budget_caches_nothing() {
        let mut lru = Lru::new();
        for k in [13, 14] {
            let value = Arc::new(k);
            assert!(Arc::ptr_eq(&lru.insert(k, value.clone(), 1, 0), &value));
            assert!(lru.get(k).is_none());
        }
        assert_eq!(lru.used, 0);
    }
}
