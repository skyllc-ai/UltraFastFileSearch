// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Canonical predicate compilation and post-filter matching.
//!
//! This module handles the translation of structured [`SearchPredicate`]s
//! from the wire protocol into the hot-path [`SearchFilters`], and provides
//! post-filter matching for predicates that cannot be compiled into the hot
//! path.

use uffs_client::protocol::response::SearchRow;
use uffs_client::protocol::{SearchPredicate, SearchPredicateOp, SearchPredicateValue};
use uffs_core::search::backend::DisplayRow;
use uffs_core::search::derived::{
    bulkiness_for_row, semantic_type_for_row, tree_allocated_for_row,
};
use uffs_core::search::field::FieldId;
use uffs_core::search::filters::SearchFilters;

use super::IndexManager;

impl IndexManager {
    /// Return whether any canonical predicates require daemon-side
    /// post-filtering.
    ///
    /// A predicate is "hot" (handled by `SearchFilters` without post-filter)
    /// when its field has `FieldAccess::Hot` and the hot-path filter pipeline
    /// already covers its operator.  Everything else needs post-filtering
    /// against the materialised `DisplayRow`.
    #[must_use]
    pub(super) fn predicates_require_post_filter(predicates: &[SearchPredicate]) -> bool {
        predicates.iter().any(|predicate| {
            let Some(field) = FieldId::parse(&predicate.field) else {
                return true;
            };
            // The hot-path `compile_predicates_into_filters` compiles these
            // field+op combinations into `SearchFilters` so they run inside the
            // compact record loop.  Anything not listed here needs post-filter.
            let compiled_to_hot_path = match field {
                // Size: Gte/Lte/Gt/Lt compiled into min_size/max_size.
                FieldId::Size => matches!(
                    predicate.op,
                    SearchPredicateOp::Gte
                        | SearchPredicateOp::Lte
                        | SearchPredicateOp::Gt
                        | SearchPredicateOp::Lt
                ),
                // Descendants: Gte/Lte/Gt/Lt compiled into min/max_descendants.
                FieldId::Descendants => matches!(
                    predicate.op,
                    SearchPredicateOp::Gte
                        | SearchPredicateOp::Lte
                        | SearchPredicateOp::Gt
                        | SearchPredicateOp::Lt
                ),
                // Timestamps: Gte/Lt compiled into newer_*/older_* bounds.
                FieldId::Modified | FieldId::Created | FieldId::Accessed => {
                    matches!(predicate.op, SearchPredicateOp::Gte | SearchPredicateOp::Lt)
                }
                // Extension: In compiled into extensions list.
                FieldId::Extension => predicate.op == SearchPredicateOp::In,
                // Attributes: HasAll/HasNone compiled into attr_require/exclude.
                FieldId::Attributes => matches!(
                    predicate.op,
                    SearchPredicateOp::HasAll | SearchPredicateOp::HasNone
                ),
                // Name: NotMatch compiled into exclude_lower glob.
                FieldId::Name => predicate.op == SearchPredicateOp::NotMatch,
                FieldId::Drive
                | FieldId::Path
                | FieldId::PathOnly
                | FieldId::SizeOnDisk
                | FieldId::Type
                | FieldId::AttributeValue
                | FieldId::Hidden
                | FieldId::System
                | FieldId::Archive
                | FieldId::ReadOnly
                | FieldId::Compressed
                | FieldId::Encrypted
                | FieldId::Sparse
                | FieldId::Reparse
                | FieldId::Offline
                | FieldId::NotIndexed
                | FieldId::Temporary
                | FieldId::Virtual
                | FieldId::Pinned
                | FieldId::Unpinned
                | FieldId::TreeSize
                | FieldId::TreeAllocated
                | FieldId::Bulkiness
                | FieldId::Integrity
                | FieldId::NoScrub
                | FieldId::DirectoryFlag
                | FieldId::RecallOnOpen
                | FieldId::RecallOnDataAccess
                | FieldId::ParityAttributes
                // WI-4.4: `malformed_path` is derived (needs the resolved
                // parent chain) → always post-filter; `name_hex` is
                // projection-only and never appears as a predicate.
                | FieldId::MalformedPath
                | FieldId::NameHex => false,
                // Length predicates are compiled into hot-path min/max filters.
                FieldId::NameLength | FieldId::PathLength => {
                    matches!(
                        predicate.op,
                        SearchPredicateOp::Gte
                            | SearchPredicateOp::Lte
                            | SearchPredicateOp::Gt
                            | SearchPredicateOp::Lt
                            | SearchPredicateOp::Eq
                    ) && matches!(predicate.value, SearchPredicateValue::U64(_))
                }
                // WI-4.4: `malformed` (leaf) compiles into the hot-path
                // `SearchFilters.malformed` toggle (Eq/Ne over a bool), so it
                // keeps the `--limit` fast path.
                FieldId::Malformed => {
                    matches!(predicate.op, SearchPredicateOp::Eq | SearchPredicateOp::Ne)
                        && matches!(predicate.value, SearchPredicateValue::Bool(_))
                }
            };
            !compiled_to_hot_path
        })
    }

    /// Overlay canonical predicates onto an existing `SearchFilters`.
    ///
    /// This compiles hot-path predicates into the compiled filter fields
    /// so they are evaluated during the fast record loop rather than in
    /// the slower post-filter pass.  Predicates that cannot be compiled
    /// into the hot path are silently skipped — they will be handled by
    /// `matches_predicate` during post-filtering.
    #[expect(
        clippy::single_call_fn,
        clippy::wildcard_enum_match_arm,
        clippy::too_many_lines,
        reason = "predicate compiler — single dispatcher with exhaustive match"
    )]
    pub(super) fn compile_predicates_into_filters(
        filters: &mut SearchFilters,
        predicates: &[SearchPredicate],
    ) {
        for predicate in predicates {
            let Some(field) = FieldId::parse(&predicate.field) else {
                continue;
            };
            match field {
                FieldId::Size => {
                    if let SearchPredicateValue::U64(val) = &predicate.value {
                        match predicate.op {
                            SearchPredicateOp::Gte => {
                                let merged = filters.min_size.map_or(*val, |cur| cur.max(*val));
                                filters.min_size = Some(merged);
                            }
                            SearchPredicateOp::Lte => {
                                let merged = filters.max_size.map_or(*val, |cur| cur.min(*val));
                                filters.max_size = Some(merged);
                            }
                            SearchPredicateOp::Gt => {
                                let lower = val.saturating_add(1);
                                let merged = filters.min_size.map_or(lower, |cur| cur.max(lower));
                                filters.min_size = Some(merged);
                            }
                            SearchPredicateOp::Lt => {
                                let upper = val.saturating_sub(1);
                                let merged = filters.max_size.map_or(upper, |cur| cur.min(upper));
                                filters.max_size = Some(merged);
                            }
                            SearchPredicateOp::Eq
                            | SearchPredicateOp::Ne
                            | SearchPredicateOp::In
                            | SearchPredicateOp::NotIn
                            | SearchPredicateOp::HasAll
                            | SearchPredicateOp::HasAny
                            | SearchPredicateOp::HasNone
                            | SearchPredicateOp::Match
                            | SearchPredicateOp::NotMatch
                            | SearchPredicateOp::Contains
                            | SearchPredicateOp::StartsWith
                            | SearchPredicateOp::EndsWith => {}
                        }
                    }
                }
                FieldId::Descendants => {
                    if let SearchPredicateValue::U64(val) = &predicate.value {
                        let val32 = u32::try_from(*val).unwrap_or(u32::MAX);
                        match predicate.op {
                            SearchPredicateOp::Gte => {
                                let merged =
                                    filters.min_descendants.map_or(val32, |cur| cur.max(val32));
                                filters.min_descendants = Some(merged);
                            }
                            SearchPredicateOp::Lte => {
                                let merged =
                                    filters.max_descendants.map_or(val32, |cur| cur.min(val32));
                                filters.max_descendants = Some(merged);
                            }
                            SearchPredicateOp::Gt => {
                                let lower = val32.saturating_add(1);
                                let merged =
                                    filters.min_descendants.map_or(lower, |cur| cur.max(lower));
                                filters.min_descendants = Some(merged);
                            }
                            SearchPredicateOp::Lt => {
                                let upper = val32.saturating_sub(1);
                                let merged =
                                    filters.max_descendants.map_or(upper, |cur| cur.min(upper));
                                filters.max_descendants = Some(merged);
                            }
                            SearchPredicateOp::Eq
                            | SearchPredicateOp::Ne
                            | SearchPredicateOp::In
                            | SearchPredicateOp::NotIn
                            | SearchPredicateOp::HasAll
                            | SearchPredicateOp::HasAny
                            | SearchPredicateOp::HasNone
                            | SearchPredicateOp::Match
                            | SearchPredicateOp::NotMatch
                            | SearchPredicateOp::Contains
                            | SearchPredicateOp::StartsWith
                            | SearchPredicateOp::EndsWith => {}
                        }
                    }
                }
                // ── Timestamp predicates (string time specs → i64 µs) ──
                FieldId::Modified | FieldId::Created | FieldId::Accessed => {
                    if let SearchPredicateValue::String(spec) = &predicate.value {
                        let now_us = uffs_core::search::filters::now_unix_micros();
                        let is_newer =
                            matches!(predicate.op, SearchPredicateOp::Gte | SearchPredicateOp::Gt);
                        if let Some(bound) =
                            uffs_core::search::filters::parse_time_bound(spec, now_us, is_newer)
                        {
                            match (field, &predicate.op) {
                                (FieldId::Modified, SearchPredicateOp::Gte) => {
                                    let merged =
                                        filters.newer_us.map_or(bound, |cur| cur.max(bound));
                                    filters.newer_us = Some(merged);
                                }
                                (FieldId::Modified, SearchPredicateOp::Lt) => {
                                    let merged =
                                        filters.older_us.map_or(bound, |cur| cur.min(bound));
                                    filters.older_us = Some(merged);
                                }
                                (FieldId::Created, SearchPredicateOp::Gte) => {
                                    let merged = filters
                                        .newer_created_us
                                        .map_or(bound, |cur| cur.max(bound));
                                    filters.newer_created_us = Some(merged);
                                }
                                (FieldId::Created, SearchPredicateOp::Lt) => {
                                    let merged = filters
                                        .older_created_us
                                        .map_or(bound, |cur| cur.min(bound));
                                    filters.older_created_us = Some(merged);
                                }
                                (FieldId::Accessed, SearchPredicateOp::Gte) => {
                                    let merged = filters
                                        .newer_accessed_us
                                        .map_or(bound, |cur| cur.max(bound));
                                    filters.newer_accessed_us = Some(merged);
                                }
                                (FieldId::Accessed, SearchPredicateOp::Lt) => {
                                    let merged = filters
                                        .older_accessed_us
                                        .map_or(bound, |cur| cur.min(bound));
                                    filters.older_accessed_us = Some(merged);
                                }
                                _ => {}
                            }
                        }
                    } else if let SearchPredicateValue::I64(val) = &predicate.value {
                        // Direct i64 timestamp µs value.
                        match (field, &predicate.op) {
                            (FieldId::Modified, SearchPredicateOp::Gte) => {
                                let merged = filters.newer_us.map_or(*val, |cur| cur.max(*val));
                                filters.newer_us = Some(merged);
                            }
                            (FieldId::Modified, SearchPredicateOp::Lt) => {
                                let merged = filters.older_us.map_or(*val, |cur| cur.min(*val));
                                filters.older_us = Some(merged);
                            }
                            (FieldId::Created, SearchPredicateOp::Gte) => {
                                let merged =
                                    filters.newer_created_us.map_or(*val, |cur| cur.max(*val));
                                filters.newer_created_us = Some(merged);
                            }
                            (FieldId::Created, SearchPredicateOp::Lt) => {
                                let merged =
                                    filters.older_created_us.map_or(*val, |cur| cur.min(*val));
                                filters.older_created_us = Some(merged);
                            }
                            (FieldId::Accessed, SearchPredicateOp::Gte) => {
                                let merged =
                                    filters.newer_accessed_us.map_or(*val, |cur| cur.max(*val));
                                filters.newer_accessed_us = Some(merged);
                            }
                            (FieldId::Accessed, SearchPredicateOp::Lt) => {
                                let merged =
                                    filters.older_accessed_us.map_or(*val, |cur| cur.min(*val));
                                filters.older_accessed_us = Some(merged);
                            }
                            _ => {}
                        }
                    }
                }
                // ── Extension predicate → hot-path ext filter ──────────
                FieldId::Extension if predicate.op == SearchPredicateOp::In => {
                    if let SearchPredicateValue::StringList(values) = &predicate.value {
                        filters.extensions.extend(values.iter().cloned());
                    }
                }
                // ── Attribute predicates → hot-path attr bitmask ───────
                FieldId::Attributes => {
                    if let SearchPredicateValue::StringList(values) = &predicate.value {
                        match predicate.op {
                            SearchPredicateOp::HasAll => {
                                for name in values {
                                    filters.attr_require |=
                                        uffs_core::search::filters::attr_bit(name);
                                }
                            }
                            SearchPredicateOp::HasNone => {
                                for name in values {
                                    filters.attr_exclude |=
                                        uffs_core::search::filters::attr_bit(name);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                // ── Exclude pattern → hot-path exclude glob ────────────
                FieldId::Name if predicate.op == SearchPredicateOp::NotMatch => {
                    if let SearchPredicateValue::String(pattern) = &predicate.value {
                        filters.exclude_lower =
                            Some(uffs_core::search::filters::fold_exclude_pattern(pattern));
                    }
                }
                // ── Name/path length → hot-path length filters ──────────
                FieldId::NameLength => {
                    if let SearchPredicateValue::U64(val) = &predicate.value {
                        let val16 = u16::try_from(*val).unwrap_or(u16::MAX);
                        match predicate.op {
                            SearchPredicateOp::Gte => {
                                let merged =
                                    filters.min_name_len.map_or(val16, |cur| cur.max(val16));
                                filters.min_name_len = Some(merged);
                            }
                            SearchPredicateOp::Lte => {
                                let merged =
                                    filters.max_name_len.map_or(val16, |cur| cur.min(val16));
                                filters.max_name_len = Some(merged);
                            }
                            SearchPredicateOp::Gt => {
                                let lower = val16.saturating_add(1);
                                let merged =
                                    filters.min_name_len.map_or(lower, |cur| cur.max(lower));
                                filters.min_name_len = Some(merged);
                            }
                            SearchPredicateOp::Lt => {
                                let upper = val16.saturating_sub(1);
                                let merged =
                                    filters.max_name_len.map_or(upper, |cur| cur.min(upper));
                                filters.max_name_len = Some(merged);
                            }
                            SearchPredicateOp::Eq => {
                                filters.min_name_len = Some(val16);
                                filters.max_name_len = Some(val16);
                            }
                            _ => {}
                        }
                    }
                }
                FieldId::PathLength => {
                    if let SearchPredicateValue::U64(val) = &predicate.value {
                        let val16 = u16::try_from(*val).unwrap_or(u16::MAX);
                        match predicate.op {
                            SearchPredicateOp::Gte => {
                                let merged =
                                    filters.min_path_len.map_or(val16, |cur| cur.max(val16));
                                filters.min_path_len = Some(merged);
                            }
                            SearchPredicateOp::Lte => {
                                let merged =
                                    filters.max_path_len.map_or(val16, |cur| cur.min(val16));
                                filters.max_path_len = Some(merged);
                            }
                            SearchPredicateOp::Gt => {
                                let lower = val16.saturating_add(1);
                                let merged =
                                    filters.min_path_len.map_or(lower, |cur| cur.max(lower));
                                filters.min_path_len = Some(merged);
                            }
                            SearchPredicateOp::Lt => {
                                let upper = val16.saturating_sub(1);
                                let merged =
                                    filters.max_path_len.map_or(upper, |cur| cur.min(upper));
                                filters.max_path_len = Some(merged);
                            }
                            SearchPredicateOp::Eq => {
                                filters.min_path_len = Some(val16);
                                filters.max_path_len = Some(val16);
                            }
                            _ => {}
                        }
                    }
                }
                // ── WI-4.4 malformed (leaf) → hot-path bool toggle ─────
                FieldId::Malformed => Self::compile_malformed(filters, predicate),
                _ => {}
            }
        }
    }

    /// Compile a `malformed` predicate into the hot-path
    /// [`SearchFilters::malformed`] toggle. `Eq true` / `Ne false` keep
    /// malformed names; `Eq false` / `Ne true` keep well-formed names. Any
    /// non-bool value or non-eq operator is ignored (the predicate then falls
    /// to the post-filter, which is a correct no-op for this field).
    const fn compile_malformed(filters: &mut SearchFilters, predicate: &SearchPredicate) {
        let SearchPredicateValue::Bool(want) = predicate.value else {
            return;
        };
        match predicate.op {
            SearchPredicateOp::Eq => filters.malformed = Some(want),
            SearchPredicateOp::Ne => filters.malformed = Some(!want),
            // All other operators are meaningless for a boolean toggle.
            SearchPredicateOp::Lt
            | SearchPredicateOp::Lte
            | SearchPredicateOp::Gt
            | SearchPredicateOp::Gte
            | SearchPredicateOp::In
            | SearchPredicateOp::NotIn
            | SearchPredicateOp::HasAll
            | SearchPredicateOp::HasAny
            | SearchPredicateOp::HasNone
            | SearchPredicateOp::Match
            | SearchPredicateOp::NotMatch
            | SearchPredicateOp::Contains
            | SearchPredicateOp::StartsWith
            | SearchPredicateOp::EndsWith => {}
        }
    }

    /// Apply canonical predicates against a materialized display row.
    #[must_use]
    pub(super) fn matches_predicates(row: &DisplayRow, predicates: &[SearchPredicate]) -> bool {
        predicates
            .iter()
            .all(|predicate| Self::matches_predicate(row, predicate))
    }

    /// Apply a single canonical predicate.
    #[must_use]
    fn matches_predicate(row: &DisplayRow, predicate: &SearchPredicate) -> bool {
        let Some(field) = FieldId::parse(&predicate.field) else {
            return true;
        };

        match field {
            FieldId::PathOnly => Self::match_string(row.path_dir(), predicate),
            FieldId::Path => Self::match_string(&row.path, predicate),
            FieldId::Name => Self::match_string(row.name(), predicate),
            FieldId::Drive => Self::match_string(&row.drive.to_string(), predicate),
            FieldId::Extension => Self::match_string(
                row.name().rsplit_once('.').map_or("", |(_, ext)| ext),
                predicate,
            ),
            FieldId::Type => Self::match_string(semantic_type_for_row(row), predicate),
            FieldId::Size => Self::match_u64(row.size, predicate),
            FieldId::SizeOnDisk => Self::match_u64(row.allocated, predicate),
            FieldId::Created => Self::match_i64(row.created, predicate),
            FieldId::Modified => Self::match_i64(row.modified, predicate),
            FieldId::Accessed => Self::match_i64(row.accessed, predicate),
            FieldId::Descendants => Self::match_u64(u64::from(row.descendants), predicate),
            FieldId::TreeSize => Self::match_u64(row.treesize, predicate),
            FieldId::TreeAllocated => Self::match_u64(tree_allocated_for_row(row), predicate),
            FieldId::Bulkiness => Self::match_u64(bulkiness_for_row(row), predicate),
            FieldId::Attributes | FieldId::AttributeValue => {
                Self::match_attributes(row.flags, predicate)
            }
            // ── Bool-typed attribute fields ─────────────────────────
            FieldId::Hidden => Self::match_bool(row.flags & 0x02 != 0, predicate),
            FieldId::System => Self::match_bool(row.flags & 0x04 != 0, predicate),
            FieldId::Archive => Self::match_bool(row.flags & 0x20 != 0, predicate),
            FieldId::ReadOnly => Self::match_bool(row.flags & 0x01 != 0, predicate),
            FieldId::Compressed => Self::match_bool(row.flags & 0x800 != 0, predicate),
            FieldId::Encrypted => Self::match_bool(row.flags & 0x4000 != 0, predicate),
            FieldId::Sparse => Self::match_bool(row.flags & 0x200 != 0, predicate),
            FieldId::Reparse => Self::match_bool(row.flags & 0x400 != 0, predicate),
            FieldId::Offline => Self::match_bool(row.flags & 0x1000 != 0, predicate),
            FieldId::NotIndexed => Self::match_bool(row.flags & 0x2000 != 0, predicate),
            FieldId::Temporary => Self::match_bool(row.flags & 0x100 != 0, predicate),
            FieldId::Virtual => Self::match_bool(row.flags & 0x0001_0000 != 0, predicate),
            FieldId::Pinned => Self::match_bool(row.flags & 0x0008_0000 != 0, predicate),
            FieldId::Unpinned => Self::match_bool(row.flags & 0x0010_0000 != 0, predicate),
            FieldId::Integrity => Self::match_bool(row.flags & 0x8000 != 0, predicate),
            FieldId::NoScrub => Self::match_bool(row.flags & 0x0002_0000 != 0, predicate),
            FieldId::DirectoryFlag => Self::match_bool(row.is_directory, predicate),
            FieldId::RecallOnOpen => {
                Self::match_bool(row.flags & Self::FLAG_RECALL_ON_OPEN != 0, predicate)
            }
            FieldId::RecallOnDataAccess => {
                Self::match_bool(row.flags & Self::FLAG_RECALL_ON_DATA_ACCESS != 0, predicate)
            }
            FieldId::ParityAttributes => {
                Self::match_u64(u64::from(row.flags & Self::PARITY_FLAG_MASK), predicate)
            }
            FieldId::NameLength => Self::match_u64(row.name().chars().count() as u64, predicate),
            FieldId::PathLength => Self::match_u64(row.path.chars().count() as u64, predicate),
            // ── WI-4.4 forensic fields ──────────────────────────────
            // `malformed` is normally compiled to the hot path; this arm is the
            // fallback when it is combined with another post-filter predicate.
            // `malformed_path` is always evaluated here (it is Derived). Both
            // read the carrier bools precomputed against the lossless bytes —
            // never recomputed from the lossy `path`. `name_hex` is not
            // filterable, so any predicate on it is a no-op (matches all).
            FieldId::Malformed => Self::match_bool(row.malformed, predicate),
            FieldId::MalformedPath => Self::match_bool(row.malformed_path, predicate),
            FieldId::NameHex => true,
        }
    }

    /// Return the extension shown to direct daemon callers.
    #[must_use]
    pub(super) fn search_row_extension(row: &SearchRow) -> &str {
        row.name.rsplit_once('.').map_or("", |(_, ext)| ext)
    }

    /// Return the semantic type shown to direct daemon callers.
    #[must_use]
    pub(super) fn search_row_type(row: &SearchRow) -> &'static str {
        if row.is_directory {
            "directory"
        } else {
            let temp = DisplayRow::new(
                0,
                row.drive,
                row.path.clone(),
                row.size,
                row.is_directory,
                row.modified,
                row.created,
                row.accessed,
                row.flags,
                row.allocated,
                row.descendants,
                row.treesize,
                row.tree_allocated,
            );
            semantic_type_for_row(&temp)
        }
    }

    /// Return the tree allocated value shown to direct daemon callers.
    #[must_use]
    pub(super) const fn search_row_tree_allocated(row: &SearchRow) -> u64 {
        if row.is_directory {
            row.tree_allocated
        } else {
            row.allocated
        }
    }

    /// Return the fixed-point bulkiness metric shown to direct daemon callers.
    #[must_use]
    pub(super) fn search_row_bulkiness(row: &SearchRow) -> u64 {
        let logical = if row.is_directory {
            row.treesize
        } else {
            row.size
        };
        let allocated = Self::search_row_tree_allocated(row);
        allocated
            .saturating_mul(1_000_000)
            .checked_div(logical)
            .unwrap_or(0)
    }

    /// Match a string predicate.
    #[must_use]
    fn match_string(actual: &str, predicate: &SearchPredicate) -> bool {
        match (&predicate.op, &predicate.value) {
            (SearchPredicateOp::Eq, SearchPredicateValue::String(expected)) => {
                actual.eq_ignore_ascii_case(expected)
            }
            (SearchPredicateOp::Ne, SearchPredicateValue::String(expected)) => {
                !actual.eq_ignore_ascii_case(expected)
            }
            (SearchPredicateOp::In, SearchPredicateValue::StringList(values)) => values
                .iter()
                .any(|value| actual.eq_ignore_ascii_case(value)),
            (SearchPredicateOp::NotIn, SearchPredicateValue::StringList(values)) => values
                .iter()
                .all(|value| !actual.eq_ignore_ascii_case(value)),
            (SearchPredicateOp::Match, SearchPredicateValue::String(pattern)) => {
                Self::wildcard_match(actual, pattern)
            }
            (SearchPredicateOp::NotMatch, SearchPredicateValue::String(pattern)) => {
                !Self::wildcard_match(actual, pattern)
            }
            // Substring containment ops — case-insensitive.
            (SearchPredicateOp::HasAll, SearchPredicateValue::StringList(values)) => {
                let lower = actual.to_ascii_lowercase();
                values
                    .iter()
                    .all(|val| lower.contains(&*val.to_ascii_lowercase()))
            }
            (SearchPredicateOp::HasAny, SearchPredicateValue::StringList(values)) => {
                let lower = actual.to_ascii_lowercase();
                values
                    .iter()
                    .any(|val| lower.contains(&*val.to_ascii_lowercase()))
            }
            (SearchPredicateOp::HasNone, SearchPredicateValue::StringList(values)) => {
                let lower = actual.to_ascii_lowercase();
                values
                    .iter()
                    .all(|val| !lower.contains(&*val.to_ascii_lowercase()))
            }
            // Substring / prefix / suffix ops.
            (SearchPredicateOp::Contains, SearchPredicateValue::String(needle)) => actual
                .to_ascii_lowercase()
                .contains(&*needle.to_ascii_lowercase()),
            (SearchPredicateOp::StartsWith, SearchPredicateValue::String(prefix)) => actual
                .to_ascii_lowercase()
                .starts_with(&*prefix.to_ascii_lowercase()),
            (SearchPredicateOp::EndsWith, SearchPredicateValue::String(suffix)) => actual
                .to_ascii_lowercase()
                .ends_with(&*suffix.to_ascii_lowercase()),
            _ => true,
        }
    }

    /// Case-insensitive wildcard match supporting `*` and `?`.
    #[must_use]
    #[expect(
        clippy::indexing_slicing,
        reason = "DP table indices are bounded by string length"
    )]
    fn wildcard_match(actual_str: &str, pattern_str: &str) -> bool {
        let actual_bytes = actual_str.to_ascii_lowercase().into_bytes();
        let pattern_bytes = pattern_str.to_ascii_lowercase().into_bytes();
        let mut dp = vec![false; actual_bytes.len() + 1];
        dp[0] = true;
        for token in pattern_bytes {
            match token {
                b'*' => {
                    let mut seen = false;
                    for slot in &mut dp {
                        seen |= *slot;
                        *slot = seen;
                    }
                }
                b'?' => {
                    for idx in (1..dp.len()).rev() {
                        dp[idx] = dp[idx - 1];
                    }
                    dp[0] = false;
                }
                byte => {
                    for idx in (1..dp.len()).rev() {
                        dp[idx] = dp[idx - 1] && actual_bytes[idx - 1] == byte;
                    }
                    dp[0] = false;
                }
            }
        }
        dp[actual_bytes.len()]
    }

    /// Match a boolean predicate.
    #[must_use]
    const fn match_bool(actual: bool, predicate: &SearchPredicate) -> bool {
        match (&predicate.op, &predicate.value) {
            (SearchPredicateOp::Eq, SearchPredicateValue::Bool(expected)) => actual == *expected,
            (SearchPredicateOp::Ne, SearchPredicateValue::Bool(expected)) => actual != *expected,
            _ => true,
        }
    }

    /// Match an unsigned numeric predicate.
    #[must_use]
    const fn match_u64(actual: u64, predicate: &SearchPredicate) -> bool {
        match (&predicate.op, &predicate.value) {
            (SearchPredicateOp::Eq, SearchPredicateValue::U64(expected)) => actual == *expected,
            (SearchPredicateOp::Ne, SearchPredicateValue::U64(expected)) => actual != *expected,
            (SearchPredicateOp::Lt, SearchPredicateValue::U64(expected)) => actual < *expected,
            (SearchPredicateOp::Lte, SearchPredicateValue::U64(expected)) => actual <= *expected,
            (SearchPredicateOp::Gt, SearchPredicateValue::U64(expected)) => actual > *expected,
            (SearchPredicateOp::Gte, SearchPredicateValue::U64(expected)) => actual >= *expected,
            _ => true,
        }
    }

    /// Match a signed numeric predicate.
    #[must_use]
    const fn match_i64(actual: i64, predicate: &SearchPredicate) -> bool {
        match (&predicate.op, &predicate.value) {
            (SearchPredicateOp::Eq, SearchPredicateValue::I64(expected)) => actual == *expected,
            (SearchPredicateOp::Ne, SearchPredicateValue::I64(expected)) => actual != *expected,
            (SearchPredicateOp::Lt, SearchPredicateValue::I64(expected)) => actual < *expected,
            (SearchPredicateOp::Lte, SearchPredicateValue::I64(expected)) => actual <= *expected,
            (SearchPredicateOp::Gt, SearchPredicateValue::I64(expected)) => actual > *expected,
            (SearchPredicateOp::Gte, SearchPredicateValue::I64(expected)) => actual >= *expected,
            _ => true,
        }
    }

    /// Match an attribute-list predicate against raw NTFS flags.
    #[must_use]
    fn match_attributes(flags: u32, predicate: &SearchPredicate) -> bool {
        let SearchPredicateValue::StringList(values) = &predicate.value else {
            return true;
        };
        match predicate.op {
            SearchPredicateOp::HasAll => values.iter().all(|name| Self::flag_set(flags, name)),
            SearchPredicateOp::HasAny => values.iter().any(|name| Self::flag_set(flags, name)),
            SearchPredicateOp::HasNone => values.iter().all(|name| !Self::flag_set(flags, name)),
            SearchPredicateOp::Eq
            | SearchPredicateOp::Ne
            | SearchPredicateOp::Lt
            | SearchPredicateOp::Lte
            | SearchPredicateOp::Gt
            | SearchPredicateOp::Gte
            | SearchPredicateOp::In
            | SearchPredicateOp::NotIn
            | SearchPredicateOp::Match
            | SearchPredicateOp::NotMatch
            | SearchPredicateOp::Contains
            | SearchPredicateOp::StartsWith
            | SearchPredicateOp::EndsWith => true,
        }
    }

    /// Test whether one named NTFS attribute bit is set in the raw flags.
    #[must_use]
    pub(super) fn flag_set(flags: u32, name: &str) -> bool {
        flags & uffs_core::search::filters::attr_bit(name) != 0
    }

    /// `FILE_ATTRIBUTE_RECALL_ON_OPEN` raw NTFS bit.
    pub(super) const FLAG_RECALL_ON_OPEN: u32 = 0x0004_0000;

    /// `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS` raw NTFS bit.
    pub(super) const FLAG_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

    /// Legacy parity mask over the raw NTFS attribute flags.
    pub(super) const PARITY_FLAG_MASK: u32 = 0x001A_EE37;
}
