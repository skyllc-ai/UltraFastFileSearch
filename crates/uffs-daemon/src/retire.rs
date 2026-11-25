// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! When an idle daemon retires: never, or after an idle stretch.

use core::time::Duration;

/// When the daemon retires on idle.
///
/// The command line spells it two ways, `--no-retire` and
/// `--idle-timeout <SECS>`; this is the one value both mean, so no reader
/// has to remember that `0` seconds is "never" (it used to retire at the
/// first idle check).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retire {
    /// Stay running until stopped (`--no-retire`, or `--idle-timeout 0`).
    Never,
    /// Retire after this much time without a client.
    AfterIdle(Duration),
}

impl Retire {
    /// The policy `--idle-timeout <idle_secs>` and `--no-retire` ask for.
    #[must_use]
    pub const fn from_flags(idle_secs: u64, no_retire: bool) -> Self {
        if no_retire || idle_secs == 0 {
            Self::Never
        } else {
            Self::AfterIdle(Duration::from_secs(idle_secs))
        }
    }

    /// The idle limit, or `None` when the daemon never retires.
    #[must_use]
    pub const fn idle_limit(self) -> Option<Duration> {
        match self {
            Self::Never => None,
            Self::AfterIdle(limit) => Some(limit),
        }
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use super::Retire;

    #[test]
    fn zero_idle_timeout_means_never() {
        assert_eq!(Retire::from_flags(0, false), Retire::Never);
        assert_eq!(Retire::from_flags(0, false).idle_limit(), None);
    }

    #[test]
    fn no_retire_wins_over_a_timeout() {
        assert_eq!(Retire::from_flags(300, true), Retire::Never);
    }

    #[test]
    fn a_timeout_is_an_idle_limit() {
        assert_eq!(
            Retire::from_flags(300, false).idle_limit(),
            Some(Duration::from_secs(300))
        );
    }
}
