// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

// Search handler: Arc clones are intentional for task-spawning; bool negation
// is clearer than `!` for readability in complex conditionals.
#![allow(
    clippy::clone_on_ref_ptr,
    clippy::if_not_else,
    reason = "search handler: Arc clones for task boundaries, readable conditionals"
)]

//! Search execution: query dispatch, profile construction, and drive info.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use uffs_client::protocol::response::{
    DriveProfile, SearchPayload, SearchProfile, SearchResponse, SearchRow,
};
use uffs_client::protocol::{SearchFilterMode, SearchParams, SearchResponseMode};
use uffs_core::search::backend::{
    DriveIndex, FilterMode, PhaseTimings, SearchRequest, SortSpec, search_index,
};
use uffs_core::search::field::FieldId;

use super::IndexManager;
pub(crate) use super::search_failure::SearchFailure;

impl IndexManager {
    /// Execute a live search query over the registry snapshot (updates perf
    /// counters). Snapshot-diff searches (`params.diff_baseline`) are routed by
    /// the handler to [`Self::diff_search`] instead, so they can surface setup
    /// errors (missing baseline / unloaded drive) as JSON-RPC errors.
    ///
    /// # Errors
    ///
    /// A [`SearchFailure`] when the scan did not complete — budget expired,
    /// search slots saturated, or the scan task panicked.  Never a
    /// success-shaped empty response.
    pub(crate) async fn search(
        &self,
        params: &SearchParams,
    ) -> Result<SearchResponse, SearchFailure> {
        self.run_search_over(params, None).await
    }

    /// Run the search pipeline over either the live registry snapshot
    /// (`snapshot_override == None`) or a caller-supplied index — the marked
    /// baseline built by [`Self::diff_search`]. When an override is present the
    /// query is a snapshot diff: the `deleted-only` filter is forced on and the
    /// registry-specific warm-up / dispatch-accounting is skipped (the baseline
    /// is not a registry shard).
    ///
    /// When `params.profile` is `true`, populates `SearchResponse::profile`
    /// with a per-phase timing breakdown so the CLI can print it.
    ///
    /// # Errors
    ///
    /// See [`Self::search`].
    #[expect(
        clippy::too_many_lines,
        reason = "search orchestration with multi-drive merge, sorting, and response formatting"
    )]
    #[expect(
        clippy::cognitive_complexity,
        reason = "search filter application with many predicate branches"
    )]
    pub(crate) async fn run_search_over(
        &self,
        params: &SearchParams,
        snapshot_override: Option<Arc<DriveIndex>>,
    ) -> Result<SearchResponse, SearchFailure> {
        let is_diff = snapshot_override.is_some();
        // Acquire a concurrency permit — blocks if too many searches
        // are already in flight.  The effective cap is
        // `max(2, (cpus × 26) / (drives × 10))` by default (see
        // `IndexManager::auto_concurrency_target`), which keeps
        // rayon-pool oversubscription from the per-query
        // `drives.par_iter()` fanout bounded to ~2.6× the machine's
        // CPU count.  Operators can clamp this down (or raise it) via
        // the `UFFS_SEARCH_MAX_CONCURRENCY` env var.
        let Some(_permit) = self.acquire_search_permit().await else {
            // Permit acquisition timed out — the global concurrency
            // cap is saturated.  Rejected before any promote was
            // attempted; the client gets an error, not "0 results".
            return Err(SearchFailure::Saturated);
        };

        let query_start = Instant::now();
        let profiling = params.profile;
        let mut effective_params = params.clone();
        effective_params.populate_canonical_fields();
        let applied_sorts = Self::resolve_applied_sorts(&effective_params);
        let projection_fields = Self::resolve_projection_fields(&effective_params.projection);
        let applied_projection: Vec<String> = projection_fields
            .iter()
            .map(|field| field.canonical_name().to_owned())
            .collect();
        let response_mode = effective_params.resolved_response_mode();
        let requires_post_filter =
            Self::predicates_require_post_filter(&effective_params.predicates);

        // Resolve sort + filter mode + filters BEFORE the promote
        // pass so Phase 4 Commit F can hand the resolved ext-term
        // list to `ensure_warm_for_dispatch` for its bloom
        // pre-check.  None of the work between here and the
        // ensure-warm call depends on the registry's tier state, so
        // the reordering is invariant-preserving.
        let (sort_column, sort_desc, extra_sort_tiers) =
            applied_sorts
                .first()
                .map_or((FieldId::Modified, true, Vec::new()), |primary| {
                    let extra = applied_sorts
                        .iter()
                        .skip(1)
                        .filter_map(Self::sort_spec_to_backend)
                        .map(|(column, descending)| SortSpec { column, descending })
                        .collect();
                    let (column, descending) =
                        Self::sort_spec_to_backend(primary).unwrap_or((FieldId::Modified, true));
                    (column, descending, extra)
                });

        let filter_mode = match effective_params.resolved_filter_mode() {
            SearchFilterMode::Files => FilterMode::FilesOnly,
            SearchFilterMode::Dirs => FilterMode::DirsOnly,
            SearchFilterMode::All => FilterMode::All,
        };

        // Resolution shared with the `warm_plan` RPC (see
        // `search_filters_build`): both must derive the SAME
        // `filters.extensions`, or an external warm gate could
        // contradict the bloom pre-check below.
        let mut filters = super::search_filters_build::build_search_filters(&effective_params);

        // Overlay canonical predicates that can be compiled into the hot
        // path (size / descendant bounds).
        Self::compile_predicates_into_filters(&mut filters, &effective_params.predicates);

        // Snapshot-diff: the override index carries the baseline with its
        // vanished rows pre-marked `DELETED`; restrict the search to those.
        if is_diff {
            filters.deleted = Some(true);
        }

        // Phase 3 Commit C — promote any Parked/Cold shards in the
        // touched set before we snapshot the active subset.  Fast
        // path (single read-lock acquisition, no work) when every
        // touched shard is already Warm/Hot, which is the common
        // case in steady state.  See
        // `IndexManager::ensure_warm_for_dispatch` doc for the
        // three-phase orchestration and the
        // conservative-on-under-promote contract.
        //
        // Phase 4 Commit F — `ext_terms` enables the bloom-skip
        // pre-check: if the user filtered by `--ext toml` and a
        // Parked shard's bloom proves it has no `.toml` records,
        // skip the promote (zero-RAM-touch contract).  Empty
        // `ext_terms` short-circuits to the Phase-3 always-promote
        // behaviour.
        // Registry warm-up only applies to live shards; a diff searches the
        // caller's baseline index, which is not in the registry.
        // Timed unconditionally, not just under `--profile`: paging a
        // cold index back in is the largest cost a query can incur, and
        // a client that cannot see it reads `duration_ms: 1` and
        // concludes the search was instant.
        let promotion_ms = if is_diff {
            0
        } else {
            let t_promote = Instant::now();
            self.ensure_warm_for_dispatch(&effective_params.drives, &filters.extensions)
                .await;
            u64::try_from(t_promote.elapsed().as_millis()).unwrap_or(u64::MAX)
        };

        // ── Snapshot the index (< 1 μs) ────────────────────────────
        let t_lock = profiling.then(Instant::now);
        let snapshot = match snapshot_override {
            Some(baseline) => baseline,
            None => self.snapshot().await,
        };
        // Phase 1 of memory-tiering: record this dispatch on every
        // active shard so `DriveStats::decay_ema` (consumed by Phase 6
        // adaptive-TTL) accumulates a real signal.  See
        // `crate::cache::DriveStats` and the `record_search_dispatch`
        // doc comment.  Skipped for a diff (the baseline is not a shard).
        if !is_diff {
            self.record_search_dispatch().await;
        }
        let lock_us = t_lock.map_or(0, |ts| ts.elapsed().as_micros());

        // Snapshot per-drive info (only when profiling).
        let drive_info: Vec<(uffs_mft::platform::DriveLetter, usize)> = if profiling {
            snapshot.drive_summary()
        } else {
            Vec::new()
        };

        // When post-filters are active the search must return more rows
        // than the user-requested limit, because some rows will be
        // discarded after path resolution.
        //
        // • Predicates (parsed `size:>1M` etc.) — unbounded: these are
        //   arbitrary user expressions so we must scan everything.
        // • Display-row filters (--in-path, --min-bulkiness, --min-path-len)
        //   — also unbounded.  The hit rate of path-based filters can be
        //   extremely low (e.g. --in-path *windows* matches <1% of files),
        //   so any fixed multiplier risks returning 0 rows.  The final
        //   limit is applied after filtering (see `filtered_rows.truncate`
        //   below).
        //
        // Build the aggregate record filter BEFORE `filters` is moved into
        // the search closure.  `type_filter` is promoted to extensions by
        // `from_params`; those same extensions must scope the aggregation.
        let agg_record_filter = uffs_core::aggregate::AggregateFilter {
            extensions: filters.extensions.clone(),
            directory_only: match filter_mode {
                FilterMode::FilesOnly => Some(false),
                FilterMode::DirsOnly => Some(true),
                FilterMode::All => None,
            },
            min_size: filters.min_size,
            max_size: filters.max_size,
        };

        // ── Aggregation routing (decided BEFORE `filters` is moved) ──
        // Rationale lives on `aggregation_needs_row_set`.
        let agg_requested = !effective_params.aggregations.is_empty();
        let agg_over_rows = aggregation_needs_row_set(&effective_params, &filters);
        // The full record-level filter set for the scan path, cloned
        // before `filters` moves into the search closure.
        let agg_search_filters = filters.clone();

        let search_limit = resolve_search_limit(
            // Aggregating over the row set needs EVERY matching row, not
            // the display limit's worth — a truncated set would silently
            // undercount.
            requires_post_filter || agg_over_rows,
            filters.needs_display_row_filter(),
            filters.malformed == Some(true),
            effective_params.limit,
        );

        // ── Execute search on a blocking thread with timeout ────────
        // `search_index` uses rayon `par_iter`, which blocks the current
        // thread.  `spawn_blocking` prevents it from starving the tokio
        // runtime.
        let pattern = effective_params.pattern.clone();
        let case_sensitive = effective_params.case_sensitive;
        let whole_word = effective_params.whole_word;
        let match_path = effective_params.match_path;
        let drives = effective_params.drives.clone();
        let agg_snapshot = snapshot.clone();
        // Cooperative cancellation: the flag travels inside `filters`
        // into every per-drive scan loop; raised below if the budget
        // expires, so the rayon workers stop instead of finishing a
        // scan nobody will read.
        let cancel = Arc::new(AtomicBool::new(false));
        filters.cancel = Some(Arc::clone(&cancel));
        let search_handle = tokio::task::spawn_blocking(move || {
            search_index(
                &snapshot,
                SearchRequest {
                    pattern: &pattern,
                    case_sensitive,
                    whole_word,
                    match_path,
                    result_limit: search_limit,
                    filter_mode,
                    search_filters: &mut filters,
                    drives_filter: &drives,
                },
                sort_column,
                sort_desc,
                &extra_sort_tiers,
            )
        });

        let budget_secs = crate::cache::policy::search_scan_budget_secs();
        let search_outcome =
            tokio::time::timeout(core::time::Duration::from_secs(budget_secs), search_handle).await;

        let result = match search_outcome {
            Ok(Ok(Ok(res))) => res,
            Ok(Ok(Err(err))) => {
                tracing::debug!(pattern = %effective_params.pattern, error = %err, "search refused");
                return Err(SearchFailure::InvalidPattern(err));
            }
            Ok(Err(_join_err)) => {
                tracing::error!(
                    pattern = %effective_params.pattern,
                    promotion_ms,
                    "search task panicked"
                );
                return Err(SearchFailure::Panicked { promotion_ms });
            }
            Err(_timeout) => {
                // Stop the workers; the dropped JoinHandle alone cannot
                // (`spawn_blocking` is not abortable).  The promotion
                // cost is reported alongside because a timeout that
                // follows an idle stretch is usually a warm-up story.
                cancel.store(true, Ordering::Release);
                tracing::warn!(
                    pattern = %effective_params.pattern,
                    budget_secs,
                    promotion_ms,
                    "search exceeded its scan budget; scan cancelled, error returned to client"
                );
                return Err(SearchFailure::TimedOut {
                    budget_secs,
                    promotion_ms,
                });
            }
        };
        let search_us = if profiling {
            result.duration.as_micros()
        } else {
            0
        };
        // Capture sub-phase timings before we consume `result.rows`.
        // `None` for non-match-all paths (regex, trigram, path-sort).
        let phase_timings = result.phase_timings;

        // ── Row building ────────────────────────────────────────────
        let t_rows = profiling.then(Instant::now);
        let mut filtered_rows = result.rows;
        if requires_post_filter {
            filtered_rows.retain(|row| Self::matches_predicates(row, &effective_params.predicates));
        }

        let mut total_count = filtered_rows.len() as u64;
        // Snapshot the matched set for row-fed aggregation BEFORE the
        // display truncation below — the display limit bounds what the
        // user sees, never what an aggregation counts.
        let agg_row_set: Vec<(uffs_mft::platform::DriveLetter, u32)> = if agg_over_rows {
            filtered_rows
                .iter()
                .map(|row| (row.drive, row.record_index))
                .collect()
        } else {
            Vec::new()
        };
        if let Some(limit) = effective_params.limit {
            filtered_rows.truncate(limit as usize);
        }

        // Per-drive match counts for `--profile`.  Computed once here
        // so both the file-sink early-return and the regular IPC path
        // share the same tally without duplicate O(N) scans.
        //
        // Single-pass map: previously this was O(rows × drives) via
        // `filter(|row| row.drive == drive).count()` inside a per-drive
        // loop.  With 4-letter drive fans and result sets in the 10⁵
        // range on validation-suite queries, the old shape
        // materialised 400 K predicate evaluations purely for
        // profiling — enough to show up in `--profile` overhead
        // measurements.  One pass over `filtered_rows` with a
        // pre-sized hash map keeps the complexity at O(rows) and
        // makes the profiling cost independent of drive count.
        let drive_match_counts: Vec<(uffs_mft::platform::DriveLetter, usize)> = if profiling {
            let mut tally: std::collections::HashMap<uffs_mft::platform::DriveLetter, usize> =
                std::collections::HashMap::with_capacity(drive_info.len().max(1));
            for row in &filtered_rows {
                *tally.entry(row.drive).or_insert(0) += 1;
            }
            // Project back to the `drive_info` ordering so callers see
            // an entry for every mounted drive (0 counts included),
            // preserving the existing contract with `SearchProfile`.
            drive_info
                .iter()
                .map(|&(drive, _records)| (drive, tally.get(&drive).copied().unwrap_or(0)))
                .collect()
        } else {
            Vec::new()
        };

        // ── Direct file output (OPT-4) ──────────────────────────────
        // When `output_file` is set, write results directly to file and
        // return metadata-only response.  Skips SearchRow allocation,
        // JSON serialization, and IPC transfer entirely.
        if let Some(output_path) = &effective_params.output_file {
            let duration_ms = u64::try_from(result.duration.as_millis()).unwrap_or(u64::MAX);
            let output_config = build_output_config(&effective_params);

            let t_write = profiling.then(Instant::now);
            // Phase 10f: `write_rows_to_file` does sync `File::create` +
            // buffered `write_all` + `rename` on the tokio runtime
            // thread.  For large result sets (10⁵+ rows × ~200 bytes ≈
            // tens of MB), the write blocks for tens-to-hundreds of ms;
            // `block_in_place` tells the multi-threaded runtime to move
            // other tasks off this worker for the duration so the IPC
            // accept loop, stats heartbeat, and per-shard journal loops
            // keep making progress.  Cheaper than `spawn_blocking` here
            // because the `Err` arm falls through to the IPC path and
            // reuses `filtered_rows` — `spawn_blocking` would force an
            // expensive clone or an `Arc<Vec<DisplayRow>>` refactor.
            let write_result = tokio::task::block_in_place(|| {
                write_rows_to_file(&filtered_rows, output_path, &output_config)
            });
            let write_us = t_write.map_or(0, |ts| ts.elapsed().as_micros());

            match write_result {
                Ok(rows_written) => {
                    tracing::info!(
                        output = output_path,
                        rows = rows_written,
                        duration_ms,
                        promotion_ms,
                        "daemon wrote results directly to file"
                    );
                    // Update perf counters.
                    let query_us = query_start.elapsed().as_micros();
                    self.queries_total.fetch_add(1, Ordering::Relaxed);
                    self.queries_total_us.fetch_add(
                        u64::try_from(query_us).unwrap_or(u64::MAX),
                        Ordering::Relaxed,
                    );
                    // Build profile so `--profile --out` can surface
                    // scan/sort/path_resolve/write_ms.  Previously this
                    // branch returned `profile: None`, hiding the new
                    // `PhaseTimings` instrumentation on the hot file
                    // benchmark path.
                    let profile = if profiling {
                        Some(
                            self.build_search_profile(
                                lock_us,
                                search_us,
                                0,
                                write_us,
                                phase_timings,
                                &drive_info,
                                &drive_match_counts,
                            )
                            .await,
                        )
                    } else {
                        None
                    };
                    return Ok(SearchResponse {
                        // File-sink path: the daemon already streamed
                        // the rows to `output_path`, so the response
                        // carries no payload — only the `rows_written`
                        // signal (via `total_count`) and timing
                        // metadata for `--profile --out`.
                        payload: SearchPayload::Empty,
                        total_count,
                        records_scanned: result.records_scanned,
                        duration_ms,
                        promotion_ms: Some(promotion_ms),
                        truncated: false,
                        profile,
                        applied_sorts: Vec::new(),
                        applied_projection: Vec::new(),
                        response_mode: None,
                        projected_rows: None,
                        aggregations: vec![],
                    });
                }
                Err(err) => {
                    tracing::error!(
                        output = output_path,
                        error = %err,
                        "failed to write results to file — falling back to IPC"
                    );
                    // Fall through to the normal IPC path.
                }
            }
        }

        // Phase 3.1 NUL fast path: skip `SearchRow` materialisation
        // when the caller explicitly opted out of row inclusion
        // (aggregate-only queries, `--no-output`, MCP facet_values,
        // etc.).  Saves an O(N) clone-and-convert pass and, once the
        // handler skips `try_pack_paths_blob`/shmem on empty rows
        // (already true), also elides ~15 ms of IPC transport for
        // medium result sets.
        //
        // `drive_match_counts` was computed up-front (see block above)
        // so both dispatch branches share the same per-drive tally.
        let filtered_len = filtered_rows.len();
        let mut rows: Vec<SearchRow> = if effective_params.include_rows {
            filtered_rows
                .iter()
                .map(Self::display_row_to_search_row)
                .collect()
        } else {
            Vec::new()
        };
        let row_build_us = t_rows.map_or(0, |ts| ts.elapsed().as_micros());
        // Opt-in, off by default (see field doc) -- every other request's
        // code path above is completely unaffected.
        rows = self
            .reorder_rows_by_physical_location(&effective_params, rows)
            .await;

        // Update perf counters.
        let query_us = query_start.elapsed().as_micros();
        self.queries_total.fetch_add(1, Ordering::Relaxed);
        self.queries_total_us.fetch_add(
            u64::try_from(query_us).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );

        // `truncated` reports whether the user's `--limit` cap was
        // reached.  Keyed off `filtered_len` (the post-filter,
        // post-truncate row count) rather than `rows.len()` so the flag
        // stays correct when `include_rows = false`.
        let truncated = effective_params
            .limit
            .is_some_and(|cap| filtered_len >= cap as usize);
        let duration_ms = u64::try_from(result.duration.as_millis()).unwrap_or(u64::MAX);

        // Profile (built in a separate method to keep `search` under the line
        // limit).
        let profile = if profiling {
            Some(
                self.build_search_profile(
                    lock_us,
                    search_us,
                    row_build_us,
                    0, // write_us: non-file-sink path, no disk write
                    phase_timings,
                    &drive_info,
                    &drive_match_counts,
                )
                .await,
            )
        } else {
            None
        };

        let projected_rows = (matches!(response_mode, SearchResponseMode::Json)
            && !projection_fields.is_empty())
        .then(|| {
            rows.iter()
                .map(|row| Self::project_search_row(row, &projection_fields))
                .collect()
        });

        // ── Aggregation (if requested) ─────────────────────────────
        let (agg_results, agg_matched) = if agg_over_rows {
            // Path-dependent query: fold the row search's matched set —
            // the rows already carry every filter and the true path
            // semantics, applied once by the engine that owns them.
            Self::run_aggregations_over_rows(
                &agg_snapshot,
                &effective_params.aggregations,
                &agg_row_set,
                build_query_predicates(&effective_params),
                effective_params.agg_cursor.as_deref(),
                effective_params.agg_page_size,
            )
        } else if agg_requested {
            let predicates = build_query_predicates(&effective_params);

            // Pass the pattern if it's non-trivial (not just `*`).
            let agg_pattern =
                if matches!(effective_params.pattern.as_str(), "*" | "**" | "**/*" | "") {
                    None
                } else {
                    Some(effective_params.pattern.as_str())
                };

            Self::run_aggregations(
                &agg_snapshot,
                Some(self.aggregate_cache()),
                &effective_params.aggregations,
                crate::index::aggregation::AggregationRequest {
                    query_predicates: predicates,
                    agg_cursor: effective_params.agg_cursor.as_deref(),
                    agg_page_size: effective_params.agg_page_size,
                    pattern: agg_pattern,
                    drives_filter: &effective_params.drives,
                    record_filter: agg_record_filter,
                    search_filters: Some(agg_search_filters),
                },
            )
        } else {
            (vec![], 0)
        };

        // When aggregation ran a filtered scan, its `records_matched`
        // gives the true total (before limit).  Otherwise use the
        // pre-truncation row count.
        if agg_matched > 0 {
            total_count = agg_matched;
        }

        // Pick the payload variant for the normal (non-file-sink)
        // path.  The search core itself only ever emits `Empty` or
        // `InlineRows` here — `handle_search` downstream may
        // re-dispatch the payload to `InlineBlob`, `ShmemBlob`, or
        // `ShmemRows` based on size + projection.
        let payload = if projected_rows.is_some() || rows.is_empty() {
            // JSON-mode callers consume `projected_rows` directly;
            // `Empty` avoids double-delivering the same data.  Zero
            // rows also collapse to `Empty` so the client doesn't
            // pay a JSON serialize of `{"kind":"inline_rows","data":[]}`.
            SearchPayload::Empty
        } else {
            SearchPayload::InlineRows(rows)
        };

        Ok(SearchResponse {
            payload,
            total_count,
            records_scanned: result.records_scanned,
            duration_ms,
            promotion_ms: Some(promotion_ms),
            truncated,
            profile,
            applied_sorts,
            applied_projection,
            response_mode: Some(response_mode),
            projected_rows,
            aggregations: agg_results,
        })
    }

    /// Build the `SearchProfile` for `--profile` output.
    ///
    /// `drive_match_counts` is pre-computed by the caller so the profile
    /// stays accurate even when row materialisation was skipped (e.g.
    /// under `--no-output`).  Pairs with `drive_info`: both slices are
    /// keyed by drive letter and have identical lengths.
    #[expect(
        clippy::too_many_arguments,
        reason = "per-phase instrumentation payload: lock/search/row_build/write + sub-phase timings + drive slices"
    )]
    async fn build_search_profile(
        &self,
        lock_us: u128,
        search_us: u128,
        row_build_us: u128,
        write_us: u128,
        phase_timings: Option<PhaseTimings>,
        drive_info: &[(uffs_mft::platform::DriveLetter, usize)],
        drive_match_counts: &[(uffs_mft::platform::DriveLetter, usize)],
    ) -> SearchProfile {
        let timings = self.drive_timings.read().await;
        let startup_us = self.startup_duration_us.load(Ordering::Relaxed);

        let us_to_ms = |us: u128| u64::try_from(us / 1000).unwrap_or(u64::MAX);
        let ms_clamp = |val: u128| u64::try_from(val).unwrap_or(u64::MAX);

        let mut drive_profiles: Vec<DriveProfile> = drive_info
            .iter()
            .map(|&(drive, records)| {
                let matches = drive_match_counts
                    .iter()
                    .find_map(|&(letter, count)| (letter == drive).then_some(count))
                    .unwrap_or(0);
                let (cache_ms, mft_ms, compact_ms, trigram_ms) =
                    timings.get(&drive).map_or((0, 0, 0, 0), |ts| {
                        (
                            ms_clamp(ts.cache),
                            ms_clamp(ts.mft),
                            ms_clamp(ts.compact),
                            ms_clamp(ts.trigram),
                        )
                    });
                DriveProfile {
                    drive,
                    records,
                    matches,
                    cache_ms,
                    mft_ms,
                    compact_ms,
                    trigram_ms,
                }
            })
            .collect();
        drive_profiles.sort_by_key(|dp| dp.drive);

        let (
            scan_ms,
            sort_ms,
            path_resolve_ms,
            path_candidates,
            path_cache_entries,
            path_resolve_fn_ns,
            path_build_row_ns,
        ) = phase_timings.map_or((0, 0, 0, 0, 0, 0, 0), |pt| {
            (
                pt.scan_ms,
                pt.sort_ms,
                pt.path_resolve_ms,
                pt.path_candidates,
                pt.path_cache_entries,
                pt.path_resolve_fn_ns,
                pt.path_build_row_ns,
            )
        });

        SearchProfile {
            uptime_ms: us_to_ms(self.start_time.elapsed().as_micros()),
            startup_ms: startup_us / 1000,
            lock_ms: us_to_ms(lock_us),
            search_ms: us_to_ms(search_us),
            row_build_ms: us_to_ms(row_build_us),
            serialize_ms: 0, // filled in by handler after shmem write
            scan_ms,
            sort_ms,
            path_resolve_ms,
            write_ms: us_to_ms(write_us),
            path_candidates,
            path_cache_entries,
            path_resolve_fn_ns,
            path_build_row_ns,
            drives: drive_profiles,
        }
    }
}

/// Decide the backend scan limit (the cap applied *before* the daemon's
/// final truncate-to-`user_limit`).
///
/// Filters evaluated *after* the per-drive scan must lift the cap
/// (`None` = unbounded) on name/regex/tree patterns — those branches cap
/// at `limit` *pattern* matches first, then filter, so a low-hit-rate
/// filter would under-return.  `requires_post_filter` (wire predicate
/// matched on the resolved `DisplayRow`) and `needs_display_row_filter`
/// (path-contains / type) both already do this.
///
/// `malformed_positive` (`--malformed`, near-zero hit rate) joins them:
/// the lift is safe for match-all because `collect_global_top_n_numeric`
/// filters *before* its bounded top-N heap.  `--well-formed` is NOT
/// lifted — the lift's `usize::MAX` flips that scan's `use_heap =
/// limit < 1M` to `false`, so its fallback `Vec` collects every survivor
/// (~the whole index here → OOM-class); it never under-returns anyway.
const fn resolve_search_limit(
    requires_post_filter: bool,
    needs_display_row_filter: bool,
    malformed_positive: bool,
    user_limit: Option<u32>,
) -> Option<u32> {
    if requires_post_filter || needs_display_row_filter || malformed_positive {
        None
    } else {
        user_limit
    }
}

// `build_output_config` lives in a sibling file to keep `search.rs`
// under the 800-line policy ceiling.  Re-exported here so every
// existing call site (`search()` below, the file-sink path, and
// `handler::RequestHandler::try_pack_csv_blob`) keeps the same
// `crate::index::search::build_output_config(...)` path — no
// call-site rename needed.
#[path = "search_output_config.rs"]
mod output_config;
pub(crate) use output_config::build_output_config;

// `build_query_predicates` lives in a sibling file to keep `search.rs`
// under the 800-line policy ceiling.  Single call site (the
// aggregation block in `search()` above), so the function stays
// `pub(super)` — not re-exported beyond the `search` module.
#[path = "search_predicates.rs"]
mod predicates;
use predicates::{aggregation_needs_row_set, build_query_predicates};

// The `--out=<path>` file-export writer lives in a sibling file to keep
// `search.rs` under the 800-line policy ceiling.  It was a `Self`-less
// associated fn, so the single call site in `search()` above invokes the
// free function directly.
#[path = "search_file_sink.rs"]
mod file_sink;
use file_sink::write_rows_to_file;

// The inline `tests` module lives in a sibling file to keep `search.rs`
// under the 800-line policy ceiling.  `#[path]` keeps the test module
// path identical (`crate::index::search::tests`), so `super::*` inside
// the tests file continues to resolve against the `search` module's
// private items (`build_output_config`, etc.).
#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;
