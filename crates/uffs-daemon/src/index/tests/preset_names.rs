// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! The preset names the client accepts (`uffs_client::format::
//! AGGREGATE_PRESETS`, which `uffs --agg --help` prints) are exactly the
//! presets the engine expands (`uffs_core::aggregate::AggregatePreset`).
//! The daemon is the one crate that sees both.

use uffs_client::format::AGGREGATE_PRESETS;
use uffs_core::aggregate::AggregatePreset;

#[test]
fn every_client_spelling_names_the_same_engine_preset() {
    for preset in AGGREGATE_PRESETS {
        let engine = AggregatePreset::parse(preset.name)
            .unwrap_or_else(|| panic!("the engine has no preset `{}`", preset.name));
        for alias in preset.aliases {
            assert_eq!(
                AggregatePreset::parse(alias),
                Some(engine),
                "`{alias}` must name the same preset as `{}`",
                preset.name
            );
        }
    }
}

#[test]
fn the_client_lists_every_engine_preset() {
    let client: Vec<&str> = AGGREGATE_PRESETS.iter().map(|preset| preset.name).collect();
    assert_eq!(client, AggregatePreset::ALL_NAMES);
}
