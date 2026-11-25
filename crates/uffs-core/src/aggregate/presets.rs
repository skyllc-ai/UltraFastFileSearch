// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Aggregate presets — named recipes that expand into multiple specs.
//!
//! Presets provide a convenient shorthand for common aggregation patterns.
//! Each preset expands into a `Vec<AggregateSpec>` that can be passed
//! directly to the aggregation engine.

use super::spec::{
    AggregateKind, AggregateSpec, BucketMetric, CalendarInterval, ScalarMetric, TopHitsSpec,
};
use crate::search::field::FieldId;

/// Named aggregate presets.
///
/// Each variant expands into a set of [`AggregateSpec`]s via
/// [`Self::expand()`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AggregatePreset {
    /// Full filesystem overview: count, files-vs-dirs, size totals,
    /// type facet, drive facet, monthly modified histogram.
    Overview,
    /// Breakdown by semantic type with size/waste metrics.
    ByType,
    /// Top extensions by count and size.
    ByExtension,
    /// Per-drive totals.
    ByDrive,
    /// Size distribution histogram.
    BySize,
    /// Age distribution by modification time.
    ByAge,
    /// Storage analysis: waste, allocated vs logical, per-drive.
    Storage,
    /// Activity: recent files, created/modified/accessed timelines.
    Activity,
    /// Top folders: path rollup at depth 1.
    TopFolders,
    /// Duplicate candidates: group by size+name.
    Duplicates,
    /// Media: pictures/audio/video summary.
    Media,
    /// Cleanup: zero-byte files, temp files, old files.
    Cleanup,
}

impl AggregatePreset {
    /// Parse a preset name from a string.
    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        match input.to_ascii_lowercase().as_str() {
            "overview" => Some(Self::Overview),
            "by_type" | "bytype" | "type" => Some(Self::ByType),
            "by_extension" | "byextension" | "extension" | "by_ext" | "ext" => {
                Some(Self::ByExtension)
            }
            "by_drive" | "bydrive" | "drive" => Some(Self::ByDrive),
            "by_size" | "bysize" | "size" => Some(Self::BySize),
            "by_age" | "byage" | "age" => Some(Self::ByAge),
            "storage" => Some(Self::Storage),
            "activity" => Some(Self::Activity),
            "top_folders" | "topfolders" | "folders" => Some(Self::TopFolders),
            "duplicates" | "dups" => Some(Self::Duplicates),
            "media" => Some(Self::Media),
            "cleanup" => Some(Self::Cleanup),
            _ => None,
        }
    }

    /// Expand this preset into a set of aggregate specs.
    #[must_use]
    pub fn expand(self) -> Vec<AggregateSpec> {
        match self {
            Self::Overview => expand_overview(),
            Self::ByType => expand_by_type(),
            Self::ByExtension => expand_by_extension(),
            Self::ByDrive => expand_by_drive(),
            Self::BySize => expand_by_size(),
            Self::ByAge => expand_by_age(),
            Self::Storage => expand_storage(),
            Self::Activity => expand_activity(),
            Self::TopFolders => expand_top_folders(),
            Self::Duplicates => expand_duplicates(),
            Self::Media => expand_media(),
            Self::Cleanup => expand_cleanup(),
        }
    }

    /// All preset names, in help-text order.
    pub const ALL_NAMES: &'static [&'static str] = &[
        "overview",
        "by_type",
        "by_extension",
        "by_drive",
        "by_size",
        "by_age",
        "storage",
        "activity",
        "top_folders",
        "duplicates",
        "media",
        "cleanup",
    ];
}

/// Overview: count + files-vs-dirs + size stats + type facet + drive facet +
/// monthly histogram.
fn expand_overview() -> Vec<AggregateSpec> {
    let default_metrics = vec![
        BucketMetric::Count,
        BucketMetric::TotalBytes,
        BucketMetric::WasteBytes,
        BucketMetric::ShareOfTotalCount,
        BucketMetric::ShareOfTotalBytes,
    ];

    vec![
        AggregateSpec::with_label(AggregateKind::Count, "total_count"),
        AggregateSpec::with_label(
            AggregateKind::Terms {
                field: FieldId::DirectoryFlag,
                top: 2,
                metrics: default_metrics.clone(),
                sample: None,
            },
            "files_vs_dirs",
        ),
        AggregateSpec::with_label(
            AggregateKind::Stats {
                field: FieldId::Size,
                metrics: vec![
                    ScalarMetric::Sum,
                    ScalarMetric::Min,
                    ScalarMetric::Max,
                    ScalarMetric::Avg,
                ],
            },
            "size_stats",
        ),
        AggregateSpec::with_label(
            AggregateKind::Terms {
                field: FieldId::Type,
                top: 30,
                metrics: default_metrics.clone(),
                sample: None,
            },
            "by_type",
        ),
        AggregateSpec::with_label(
            AggregateKind::Terms {
                field: FieldId::Drive,
                top: 26,
                metrics: default_metrics,
                sample: None,
            },
            "by_drive",
        ),
        AggregateSpec::with_label(
            AggregateKind::DateHistogram {
                field: FieldId::Modified,
                calendar: CalendarInterval::Month,
                metrics: vec![BucketMetric::Count, BucketMetric::TotalBytes],
            },
            "modified_monthly",
        ),
    ]
}

/// By type: terms on type with size/waste metrics.
fn expand_by_type() -> Vec<AggregateSpec> {
    vec![AggregateSpec::with_label(
        AggregateKind::Terms {
            field: FieldId::Type,
            top: 30,
            metrics: vec![
                BucketMetric::Count,
                BucketMetric::TotalBytes,
                BucketMetric::TotalAllocated,
                BucketMetric::WasteBytes,
                BucketMetric::WastePct,
                BucketMetric::AvgSize,
                BucketMetric::ShareOfTotalCount,
                BucketMetric::ShareOfTotalBytes,
            ],
            sample: None,
        },
        "by_type",
    )]
}

/// By extension: top 50 extensions by count with size metrics.
fn expand_by_extension() -> Vec<AggregateSpec> {
    vec![AggregateSpec::with_label(
        AggregateKind::Terms {
            field: FieldId::Extension,
            top: 50,
            metrics: vec![
                BucketMetric::Count,
                BucketMetric::TotalBytes,
                BucketMetric::AvgSize,
                BucketMetric::ShareOfTotalCount,
                BucketMetric::ShareOfTotalBytes,
            ],
            sample: None,
        },
        "by_extension",
    )]
}

/// By drive: terms on drive with size totals.
fn expand_by_drive() -> Vec<AggregateSpec> {
    vec![AggregateSpec::with_label(
        AggregateKind::Terms {
            field: FieldId::Drive,
            top: 26,
            metrics: vec![
                BucketMetric::Count,
                BucketMetric::TotalBytes,
                BucketMetric::TotalAllocated,
                BucketMetric::WasteBytes,
                BucketMetric::WastePct,
                BucketMetric::ShareOfTotalCount,
                BucketMetric::ShareOfTotalBytes,
            ],
            sample: None,
        },
        "by_drive",
    )]
}

/// By size: histogram with predefined size buckets.
fn expand_by_size() -> Vec<AggregateSpec> {
    // Use the size bucket boundaries as range boundaries.
    let boundaries = vec![
        1_024,          // 1 KB
        102_400,        // 100 KB
        1_048_576,      // 1 MB
        104_857_600,    // 100 MB
        1_073_741_824,  // 1 GB
        10_737_418_240, // 10 GB
    ];

    vec![
        AggregateSpec::with_label(
            AggregateKind::Range {
                field: FieldId::Size,
                boundaries,
                metrics: vec![
                    BucketMetric::Count,
                    BucketMetric::TotalBytes,
                    BucketMetric::ShareOfTotalCount,
                    BucketMetric::ShareOfTotalBytes,
                ],
            },
            "by_size",
        ),
        AggregateSpec::with_label(
            AggregateKind::Stats {
                field: FieldId::Size,
                metrics: vec![
                    ScalarMetric::Sum,
                    ScalarMetric::Min,
                    ScalarMetric::Max,
                    ScalarMetric::Avg,
                ],
            },
            "size_totals",
        ),
    ]
}

/// By age: date histogram on modified with monthly buckets.
fn expand_by_age() -> Vec<AggregateSpec> {
    vec![AggregateSpec::with_label(
        AggregateKind::DateHistogram {
            field: FieldId::Modified,
            calendar: CalendarInterval::Month,
            metrics: vec![
                BucketMetric::Count,
                BucketMetric::TotalBytes,
                BucketMetric::ShareOfTotalCount,
                BucketMetric::ShareOfTotalBytes,
            ],
        },
        "by_age",
    )]
}

/// Storage: waste analysis + allocated vs logical + per-drive breakdown.
fn expand_storage() -> Vec<AggregateSpec> {
    vec![
        AggregateSpec::with_label(
            AggregateKind::Stats {
                field: FieldId::Size,
                metrics: vec![ScalarMetric::Sum],
            },
            "logical_size",
        ),
        AggregateSpec::with_label(
            AggregateKind::Stats {
                field: FieldId::SizeOnDisk,
                metrics: vec![ScalarMetric::Sum],
            },
            "allocated_size",
        ),
        AggregateSpec::with_label(
            AggregateKind::Terms {
                field: FieldId::Drive,
                top: 26,
                metrics: vec![
                    BucketMetric::Count,
                    BucketMetric::TotalBytes,
                    BucketMetric::TotalAllocated,
                    BucketMetric::WasteBytes,
                    BucketMetric::WastePct,
                ],
                sample: None,
            },
            "waste_by_drive",
        ),
        AggregateSpec::with_label(
            AggregateKind::Terms {
                field: FieldId::Extension,
                top: 20,
                metrics: vec![
                    BucketMetric::Count,
                    BucketMetric::TotalBytes,
                    BucketMetric::WasteBytes,
                    BucketMetric::WastePct,
                ],
                sample: None,
            },
            "waste_by_extension",
        ),
    ]
}

/// Activity: recent files + creation/modification timelines.
fn expand_activity() -> Vec<AggregateSpec> {
    vec![
        AggregateSpec::with_label(
            AggregateKind::DateHistogram {
                field: FieldId::Modified,
                calendar: CalendarInterval::Month,
                metrics: vec![BucketMetric::Count, BucketMetric::TotalBytes],
            },
            "modified_monthly",
        ),
        AggregateSpec::with_label(
            AggregateKind::DateHistogram {
                field: FieldId::Created,
                calendar: CalendarInterval::Month,
                metrics: vec![BucketMetric::Count, BucketMetric::TotalBytes],
            },
            "created_monthly",
        ),
        AggregateSpec::with_label(
            AggregateKind::DateHistogram {
                field: FieldId::Accessed,
                calendar: CalendarInterval::Month,
                metrics: vec![BucketMetric::Count],
            },
            "accessed_monthly",
        ),
    ]
}

/// Top folders: path rollup at depth 1.
fn expand_top_folders() -> Vec<AggregateSpec> {
    use super::spec::RollupMode;
    vec![AggregateSpec::with_label(
        AggregateKind::Rollup {
            mode: RollupMode::Path { depth: 1 },
            top: 30,
            metrics: vec![
                BucketMetric::Count,
                BucketMetric::TotalBytes,
                BucketMetric::TotalAllocated,
                BucketMetric::WasteBytes,
                BucketMetric::ShareOfTotalBytes,
            ],
            sample: None,
            sub: None,
        },
        "top_folders",
    )]
}

/// Duplicate candidates: group by size+name.
fn expand_duplicates() -> Vec<AggregateSpec> {
    use super::spec::DuplicateVerify;
    vec![AggregateSpec::with_label(
        AggregateKind::Duplicates {
            keys: vec![FieldId::Size, FieldId::Name],
            verify: DuplicateVerify::None,
            top: 100,
            sample: Some(TopHitsSpec::with_count(2)),
            max_groups: 500_000,
        },
        "duplicate_candidates",
    )]
}

/// Media: pictures/audio/video summary with type facet + size + age.
fn expand_media() -> Vec<AggregateSpec> {
    vec![
        AggregateSpec::with_label(
            AggregateKind::Terms {
                field: FieldId::Type,
                top: 12,
                metrics: vec![
                    BucketMetric::Count,
                    BucketMetric::TotalBytes,
                    BucketMetric::AvgSize,
                    BucketMetric::ShareOfTotalBytes,
                ],
                sample: None,
            },
            "media_type_breakdown",
        ),
        AggregateSpec::with_label(
            AggregateKind::Stats {
                field: FieldId::Size,
                metrics: vec![ScalarMetric::Sum, ScalarMetric::Avg, ScalarMetric::Max],
            },
            "media_size_stats",
        ),
        AggregateSpec::with_label(
            AggregateKind::Terms {
                field: FieldId::Extension,
                top: 30,
                metrics: vec![
                    BucketMetric::Count,
                    BucketMetric::TotalBytes,
                    BucketMetric::ShareOfTotalBytes,
                ],
                sample: None,
            },
            "media_extensions",
        ),
        AggregateSpec::with_label(
            AggregateKind::DateHistogram {
                field: FieldId::Created,
                calendar: CalendarInterval::Month,
                metrics: vec![BucketMetric::Count, BucketMetric::TotalBytes],
            },
            "media_created_monthly",
        ),
    ]
}

/// Cleanup: missing extensions, zero-byte files, large temp files.
fn expand_cleanup() -> Vec<AggregateSpec> {
    vec![
        AggregateSpec::with_label(
            AggregateKind::Missing {
                field: FieldId::Extension,
            },
            "no_extension",
        ),
        AggregateSpec::with_label(
            AggregateKind::Missing {
                field: FieldId::Size,
            },
            "zero_byte_files",
        ),
        AggregateSpec::with_label(
            AggregateKind::Distinct {
                field: FieldId::Extension,
            },
            "distinct_extensions",
        ),
        AggregateSpec::with_label(AggregateKind::Count, "total_files"),
    ]
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "tests assert against fixtures with known shape; indexing panic = test failure"
)]
mod tests {
    use super::*;

    #[test]
    fn parse_all_presets() {
        for &name in AggregatePreset::ALL_NAMES {
            assert!(
                AggregatePreset::parse(name).is_some(),
                "Failed to parse preset: {name}"
            );
        }
    }

    #[test]
    fn parse_invalid_preset() {
        assert!(AggregatePreset::parse("nonexistent").is_none());
    }

    #[test]
    fn overview_expansion_has_expected_specs() {
        let specs = AggregatePreset::Overview.expand();
        assert!(specs.len() >= 5, "overview should have at least 5 specs");

        // Check labels.
        let labels: Vec<_> = specs
            .iter()
            .filter_map(|spec| spec.label.as_deref())
            .collect();
        assert!(labels.contains(&"total_count"));
        assert!(labels.contains(&"files_vs_dirs"));
        assert!(labels.contains(&"size_stats"));
        assert!(labels.contains(&"by_type"));
        assert!(labels.contains(&"by_drive"));
    }

    #[test]
    fn by_type_expansion() {
        let specs = AggregatePreset::ByType.expand();
        assert_eq!(specs.len(), 1);
        if let AggregateKind::Terms { field, top, .. } = &specs[0].kind {
            assert_eq!(*field, FieldId::Type);
            assert_eq!(*top, 30);
        } else {
            panic!("expected Terms");
        }
    }

    #[test]
    fn by_extension_expansion() {
        let specs = AggregatePreset::ByExtension.expand();
        assert_eq!(specs.len(), 1);
        if let AggregateKind::Terms { field, top, .. } = &specs[0].kind {
            assert_eq!(*field, FieldId::Extension);
            assert_eq!(*top, 50);
        } else {
            panic!("expected Terms");
        }
    }

    #[test]
    fn by_size_expansion() {
        let specs = AggregatePreset::BySize.expand();
        assert_eq!(specs.len(), 2);
        // First should be a Range, second should be Stats.
        assert!(matches!(specs[0].kind, AggregateKind::Range { .. }));
        assert!(matches!(specs[1].kind, AggregateKind::Stats { .. }));
    }

    #[test]
    fn by_age_expansion() {
        let specs = AggregatePreset::ByAge.expand();
        assert_eq!(specs.len(), 1);
        assert!(matches!(specs[0].kind, AggregateKind::DateHistogram { .. }));
    }

    #[test]
    fn all_presets_produce_valid_specs() {
        use crate::aggregate::planner::AggregatePlan;

        for &name in AggregatePreset::ALL_NAMES {
            let preset = AggregatePreset::parse(name).expect("valid preset");
            let specs = preset.expand();
            let result = AggregatePlan::compile(&specs);
            assert!(
                result.is_ok(),
                "preset {name} produced invalid specs: {:?}",
                result.err()
            );
        }
    }
}
