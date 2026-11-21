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

//! `--num-workers`: how many proofs the server proves at once.

use std::fmt;
use std::num::NonZeroUsize;
use std::str::FromStr;

use tracing::info;

/// Default worker count. A client sends a transaction's proofs together, and GPU
/// builds prove them concurrently on one device (most of its memory, the NTT tables
/// and the parameters' bases, is shared between proofs) while their CPU phases
/// share the host's CPUs, so they default to one worker per available CPU.
#[cfg(feature = "gpu")]
pub(crate) const DEFAULT_NUM_WORKERS: NumWorkers = NumWorkers::Auto;
#[cfg(not(feature = "gpu"))]
pub(crate) const DEFAULT_NUM_WORKERS: NumWorkers = NumWorkers::Count(2);

/// The fewest workers `auto` chooses (upstream's default).
const MIN_AUTO_WORKERS: usize = 2;

/// `--num-workers`: a count, or `auto` for one worker per CPU available to the
/// process (at least [`MIN_AUTO_WORKERS`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NumWorkers {
    Auto,
    Count(usize),
}

impl NumWorkers {
    /// The worker count, given the CPUs available to the process (`None` when
    /// unknown).
    fn resolve(self, cpus: Option<NonZeroUsize>) -> usize {
        match self {
            Self::Count(n) => n,
            Self::Auto => cpus
                .map_or(MIN_AUTO_WORKERS, NonZeroUsize::get)
                .max(MIN_AUTO_WORKERS),
        }
    }

    /// The worker count for this process, logged with how it was chosen.
    pub(crate) fn for_this_process(self) -> usize {
        let cpus = std::thread::available_parallelism().ok();
        let num_workers = self.resolve(cpus);
        match self {
            Self::Auto => info!(
                "Proof workers: {num_workers} (auto: one per available CPU, {} available)",
                cpus.map_or_else(|| "unknown".to_owned(), |n| n.to_string())
            ),
            Self::Count(_) => info!("Proof workers: {num_workers}"),
        }
        num_workers
    }
}

impl FromStr for NumWorkers {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.eq_ignore_ascii_case("auto") {
            return Ok(Self::Auto);
        }
        s.parse()
            .map(Self::Count)
            .map_err(|_| format!("expected a number or `auto`, got `{s}`"))
    }
}

impl fmt::Display for NumWorkers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => f.write_str("auto"),
            Self::Count(n) => write!(f, "{n}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Args;
    use clap::Parser;

    fn cpus(n: usize) -> Option<NonZeroUsize> {
        NonZeroUsize::new(n)
    }

    #[test]
    fn auto_is_one_worker_per_available_cpu_and_at_least_the_minimum() {
        assert_eq!(NumWorkers::Auto.resolve(cpus(8)), 8);
        assert_eq!(NumWorkers::Auto.resolve(cpus(64)), 64);
        assert_eq!(NumWorkers::Auto.resolve(cpus(1)), MIN_AUTO_WORKERS);
        assert_eq!(NumWorkers::Auto.resolve(None), MIN_AUTO_WORKERS);
    }

    #[test]
    fn an_explicit_count_is_used_as_given() {
        assert_eq!(NumWorkers::Count(3).resolve(cpus(8)), 3);
        assert_eq!(NumWorkers::Count(12).resolve(None), 12);
    }

    #[test]
    fn num_workers_parses_a_count_or_auto() {
        assert_eq!("auto".parse(), Ok(NumWorkers::Auto));
        assert_eq!("AUTO".parse(), Ok(NumWorkers::Auto));
        assert_eq!("4".parse(), Ok(NumWorkers::Count(4)));
        assert!("".parse::<NumWorkers>().is_err());
        assert!("-1".parse::<NumWorkers>().is_err());
        assert!("four".parse::<NumWorkers>().is_err());
        for value in [NumWorkers::Auto, NumWorkers::Count(6)] {
            assert_eq!(value.to_string().parse(), Ok(value), "{value} round-trips");
        }
    }

    #[test]
    fn the_command_line_takes_auto_or_a_count_and_defaults_by_build() {
        let parse = |args: &[&str]| Args::try_parse_from(args).map(|a| a.num_workers);
        assert_eq!(
            parse(&["server", "--num-workers", "auto"]).unwrap(),
            NumWorkers::Auto
        );
        assert_eq!(
            parse(&["server", "--num-workers", "5"]).unwrap(),
            NumWorkers::Count(5)
        );
        assert!(parse(&["server", "--num-workers", "many"]).is_err());
        if std::env::var_os("MIDNIGHT_PROOF_SERVER_NUM_WORKERS").is_none() {
            let expected = if cfg!(feature = "gpu") {
                NumWorkers::Auto
            } else {
                NumWorkers::Count(2)
            };
            assert_eq!(parse(&["server"]).unwrap(), expected);
        }
    }
}
