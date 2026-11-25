// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Unit tests for `uffs-mcp` lib.
//!
//! Extracted from `lib.rs` via `#[path]` so the entry-point file
//! stays under the workspace 800-LOC ceiling.  All tests are
//! identical to their previous in-line form; module structure (the
//! seven nested `*_tests` submodules) is preserved so any
//! `super::INVALID_PARAMS` / `super::INTERNAL_ERROR` references keep
//! resolving the same way.

/// JSON-RPC 2.0 error code for "Invalid params".
const INVALID_PARAMS: rmcp::model::ErrorCode = rmcp::model::ErrorCode(-32602);
/// JSON-RPC 2.0 error code for "Internal error".
const INTERNAL_ERROR: rmcp::model::ErrorCode = rmcp::model::ErrorCode(-32603);

mod error_tests {
    use rmcp::ErrorData as McpError;

    use crate::error::BridgeError;

    #[test]
    fn missing_param_maps_to_invalid_params() {
        let bridge = BridgeError::MissingParam("pattern");
        let mcp: McpError = McpError::from(bridge);
        assert_eq!(mcp.code, super::INVALID_PARAMS);
        assert!(mcp.message.contains("pattern"));
    }

    #[test]
    fn invalid_param_maps_to_invalid_params() {
        let bridge = BridgeError::InvalidParam {
            name: "limit",
            reason: "must be positive".to_owned(),
        };
        let mcp: McpError = McpError::from(bridge);
        assert_eq!(mcp.code, super::INVALID_PARAMS);
        assert!(mcp.message.contains("limit"));
        assert!(mcp.message.contains("must be positive"));
    }

    #[test]
    fn daemon_error_maps_to_internal() {
        let bridge = BridgeError::Daemon("connection reset".to_owned());
        let mcp: McpError = McpError::from(bridge);
        assert_eq!(mcp.code, super::INTERNAL_ERROR);
        assert!(mcp.message.contains("connection reset"));
    }

    #[test]
    fn serialization_error_maps_to_internal() {
        let serde_err = serde_json::from_str::<serde_json::Value>("{bad}").unwrap_err();
        let bridge: BridgeError = serde_err.into();
        let mcp: McpError = McpError::from(bridge);
        assert_eq!(mcp.code, super::INTERNAL_ERROR);
    }
}

mod text_tests {
    use uffs_client::protocol::response::SearchRow;

    use crate::text::format_search_row;

    fn test_row(name: &str, size: u64, modified: i64, path: &str) -> SearchRow {
        SearchRow {
            drive: uffs_mft::platform::DriveLetter::C,
            name: name.to_owned(),
            size,
            is_directory: false,
            modified,
            created: 0,
            accessed: 0,
            flags: 0x20,
            allocated: size,
            path: path.to_owned(),
            descendants: 0,
            treesize: 0,
            tree_allocated: 0,
            malformed: false,
            malformed_path: false,
            name_hex: None,
            file_reference: 0,
        }
    }

    #[test]
    fn format_row_basic() {
        // 2024-01-15 as FILETIME (raw 100-ns ticks since 1601-01-01)
        let ft_2024_01_15 = 1_705_312_200_000_000_i64 * 10 + 116_444_736_000_000_000;
        let row = test_row("hello.rs", 1024, ft_2024_01_15, "C:\\src\\hello.rs");
        let formatted = format_search_row(&row);
        assert!(formatted.contains("hello.rs"), "name: {formatted}");
        assert!(formatted.contains("1.0 KB"), "size: {formatted}");
        assert!(formatted.contains("2024-01-15"), "date: {formatted}");
        assert!(formatted.contains("C:\\src\\hello.rs"), "path: {formatted}");
    }

    #[test]
    fn format_row_large_size() {
        let row = test_row(
            "big.bin",
            1_073_741_824,
            // 2023-11-14 as FILETIME
            1_700_000_000_000_000_i64 * 10 + 116_444_736_000_000_000,
            "D:\\data\\big.bin",
        );
        let formatted = format_search_row(&row);
        assert!(formatted.contains("big.bin"));
        assert!(formatted.contains("1.0 GB"), "size: {formatted}");
    }

    #[test]
    fn format_row_zero_timestamp() {
        let row = test_row("", 0, 0, "");
        let formatted = format_search_row(&row);
        // Should still produce valid markdown table row
        assert!(formatted.starts_with('|'));
        // Zero timestamp renders as "—"
        assert!(formatted.contains('—'), "zero ts: {formatted}");
    }
}

mod prompt_tests {
    use crate::handler::prompts::{build_prompt_messages, str_arg, u64_arg};

    #[test]
    fn str_arg_extracts_string() {
        let mut map = serde_json::Map::new();
        map.insert("key".to_owned(), serde_json::json!("value"));
        assert_eq!(str_arg(&map, "key"), Some("value"));
    }

    #[test]
    fn str_arg_returns_none_for_missing() {
        let map = serde_json::Map::new();
        assert_eq!(str_arg(&map, "missing"), None);
    }

    #[test]
    fn str_arg_returns_none_for_non_string() {
        let mut map = serde_json::Map::new();
        map.insert("key".to_owned(), serde_json::json!(42_i32));
        assert_eq!(str_arg(&map, "key"), None);
    }

    #[test]
    fn u64_arg_parses_numeric_string() {
        let mut map = serde_json::Map::new();
        map.insert("limit".to_owned(), serde_json::json!("25"));
        assert_eq!(u64_arg(&map, "limit", 50), 25);
    }

    #[test]
    fn u64_arg_uses_default_when_missing() {
        let map = serde_json::Map::new();
        assert_eq!(u64_arg(&map, "limit", 50), 50);
    }

    #[test]
    fn u64_arg_uses_default_when_not_numeric() {
        let mut map = serde_json::Map::new();
        map.insert("limit".to_owned(), serde_json::json!("abc"));
        assert_eq!(u64_arg(&map, "limit", 50), 50);
    }

    // ── build_prompt_messages tests ─────────────────────────────

    #[test]
    fn find_large_files_default_limit() {
        let args = serde_json::Map::new();
        let msgs = build_prompt_messages("find_large_files", &args).unwrap();
        assert_eq!(msgs.len(), 1);
        let text = format!("{:?}", *msgs.first().expect("msgs has an element 0"));
        assert!(text.contains("50"), "default limit 50: {text}");
    }

    #[test]
    fn find_large_files_custom_limit() {
        let mut args = serde_json::Map::new();
        args.insert("limit".to_owned(), serde_json::json!("10"));
        let msgs = build_prompt_messages("find_large_files", &args).unwrap();
        let text = format!("{:?}", *msgs.first().expect("msgs has an element 0"));
        assert!(text.contains("10"), "custom limit: {text}");
    }

    #[test]
    fn recent_changes_default() {
        let args = serde_json::Map::new();
        let msgs = build_prompt_messages("recent_changes", &args).unwrap();
        let text = format!("{:?}", *msgs.first().expect("msgs has an element 0"));
        assert!(text.contains("1 day"), "default 1 day: {text}");
    }

    #[test]
    fn find_by_extension_with_ext() {
        let mut args = serde_json::Map::new();
        args.insert("extension".to_owned(), serde_json::json!("pdf"));
        let msgs = build_prompt_messages("find_by_extension", &args).unwrap();
        let text = format!("{:?}", *msgs.first().expect("msgs has an element 0"));
        assert!(text.contains("*.pdf"), "pdf pattern: {text}");
    }

    #[test]
    fn disk_usage_report_with_drive() {
        let mut args = serde_json::Map::new();
        args.insert("drive".to_owned(), serde_json::json!("C"));
        let msgs = build_prompt_messages("disk_usage_report", &args).unwrap();
        let text = format!("{:?}", *msgs.first().expect("msgs has an element 0"));
        assert!(text.contains("drive C:"), "drive scope: {text}");
        assert!(text.contains("Step 1"), "multi-step: {text}");
    }

    #[test]
    fn cleanup_report_custom_size() {
        let mut args = serde_json::Map::new();
        args.insert("min_size_mb".to_owned(), serde_json::json!("500"));
        let msgs = build_prompt_messages("cleanup_report", &args).unwrap();
        let text = format!("{:?}", *msgs.first().expect("msgs has an element 0"));
        assert!(text.contains("500MB"), "custom min_size: {text}");
    }

    #[test]
    fn duplicate_investigation_with_ext() {
        let mut args = serde_json::Map::new();
        args.insert("extension".to_owned(), serde_json::json!("jpg"));
        let msgs = build_prompt_messages("duplicate_investigation", &args).unwrap();
        let text = format!("{:?}", *msgs.first().expect("msgs has an element 0"));
        assert!(text.contains("*.jpg"), "ext filter: {text}");
    }

    #[test]
    fn unknown_prompt_returns_error() {
        let args = serde_json::Map::new();
        let result = build_prompt_messages("nonexistent_prompt", &args);
        result.unwrap_err();
    }

    #[test]
    fn all_7_prompts_are_defined() {
        let defs = crate::handler::definitions::prompt_definitions();
        assert_eq!(defs.len(), 7, "expected 7 prompts, got {}", defs.len());

        let names: Vec<_> = defs.iter().map(|prompt| prompt.name.as_ref()).collect();
        assert!(names.contains(&"find_large_files"));
        assert!(names.contains(&"recent_changes"));
        assert!(names.contains(&"find_by_extension"));
        assert!(names.contains(&"find_duplicates_by_name"));
        assert!(names.contains(&"disk_usage_report"));
        assert!(names.contains(&"cleanup_report"));
        assert!(names.contains(&"duplicate_investigation"));
    }
}

// ── tool definition tests ───────────────────────────────────────

mod tool_def_tests {
    use crate::handler::definitions::tool_definitions;

    #[test]
    fn six_tools_defined() {
        let tools = tool_definitions();
        assert_eq!(tools.len(), 6, "expected 6 tools, got {}", tools.len());
    }

    #[test]
    fn tool_names_are_namespaced() {
        let tools = tool_definitions();
        for tool in &tools {
            assert!(
                tool.name.starts_with("uffs_"),
                "tool '{}' should be namespaced",
                tool.name
            );
        }
    }

    #[test]
    fn expected_tools_present() {
        let tools = tool_definitions();
        let names: Vec<_> = tools.iter().map(|tool| tool.name.as_ref()).collect();
        for expected in &[
            "uffs_search",
            "uffs_info",
            "uffs_drives",
            "uffs_status",
            "uffs_aggregate",
            "uffs_facet_values",
        ] {
            assert!(names.contains(expected), "missing tool: {expected}");
        }
    }

    #[test]
    fn all_tools_have_descriptions() {
        let tools = tool_definitions();
        for tool in &tools {
            let desc = tool.description.as_deref().unwrap_or("");
            assert!(
                !desc.is_empty(),
                "tool '{}' has empty description",
                tool.name
            );
        }
    }
}

// ── tool args deserialization tests ──────────────────────────────

mod tool_args_tests {
    use crate::tools::aggregate::AggregateArgs;
    use crate::tools::facet_values::FacetValuesArgs;
    use crate::tools::info::InfoArgs;
    use crate::tools::search::SearchArgs;

    #[test]
    fn search_args_defaults() {
        let args: SearchArgs = serde_json::from_value(serde_json::json!({
            "pattern": "*.rs"
        }))
        .unwrap();
        assert_eq!(args.pattern, "*.rs");
        assert!(!args.case_sensitive);
        assert_eq!(args.sort, "modified");
        assert!(
            !args.sort_desc,
            "sort_desc defaults to false (ascending), matching CLI"
        );
        assert_eq!(args.limit, 50);
        assert_eq!(args.filter, "all");
    }

    #[test]
    fn search_args_custom_values() {
        let args: SearchArgs = serde_json::from_value(serde_json::json!({
            "pattern": ">report_[0-9]+",
            "case_sensitive": true,
            "sort": "size",
            "sort_desc": false,
            "limit": 25_i32,
            "filter": "files"
        }))
        .unwrap();
        assert_eq!(args.pattern, ">report_[0-9]+");
        assert!(args.case_sensitive);
        assert_eq!(args.sort, "size");
        assert!(!args.sort_desc);
        assert_eq!(args.limit, 25);
        assert_eq!(args.filter, "files");
    }

    #[test]
    fn aggregate_args_defaults() {
        let args: AggregateArgs = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(args.pattern, "*");
        assert!(args.preset.is_none());
        assert_eq!(args.aggregations, Vec::<String>::new());
        assert_eq!(args.drives, Vec::<String>::new());
    }

    #[test]
    fn aggregate_args_with_preset() {
        let args: AggregateArgs = serde_json::from_value(serde_json::json!({
            "preset": "by_extension",
            "drives": ["C", "D"]
        }))
        .unwrap();
        assert_eq!(args.preset.as_deref(), Some("by_extension"));
        assert_eq!(args.drives, vec!["C", "D"]);
    }

    #[test]
    fn facet_values_args_defaults() {
        let args: FacetValuesArgs = serde_json::from_value(serde_json::json!({
            "field": "extension"
        }))
        .unwrap();
        assert_eq!(args.field, "extension");
        assert_eq!(args.top, 20);
        assert!(args.prefix.is_none());
    }

    #[test]
    fn info_args_basic() {
        let args: InfoArgs = serde_json::from_value(serde_json::json!({
            "path": "C:\\Windows\\System32\\notepad.exe"
        }))
        .unwrap();
        assert_eq!(args.path, "C:\\Windows\\System32\\notepad.exe");
    }
}

// ── percent encode/decode round-trip ────────────────────────────

mod percent_encode_tests {
    use crate::handler::definitions::percent_decode_path;
    use crate::tools::search::percent_encode_path;

    #[test]
    fn round_trip_simple_path() {
        let path = r"C:\Users\me\project\file.rs";
        let encoded = percent_encode_path(path);
        assert_eq!(encoded, "C:/Users/me/project/file.rs");
        let decoded = percent_decode_path(&encoded);
        // Decode gives forward slashes; the handler normalises back.
        assert_eq!(decoded, "C:/Users/me/project/file.rs");
    }

    #[test]
    fn round_trip_path_with_spaces() {
        let path = r"C:\Program Files\My App\data.txt";
        let encoded = percent_encode_path(path);
        assert!(
            encoded.contains("%20"),
            "spaces should be encoded: {encoded}"
        );
        let decoded = percent_decode_path(&encoded);
        assert_eq!(decoded, "C:/Program Files/My App/data.txt");
    }

    #[test]
    fn round_trip_path_with_unicode() {
        let path = r"D:\文档\报告.pdf";
        let encoded = percent_encode_path(path);
        let decoded = percent_decode_path(&encoded);
        assert_eq!(decoded, "D:/文档/报告.pdf");
    }

    #[test]
    fn decode_passthrough_for_unencoded() {
        assert_eq!(percent_decode_path("C:/simple/path"), "C:/simple/path");
    }

    #[test]
    fn decode_handles_percent_at_end() {
        // Truncated percent sequence should be passed through.
        assert_eq!(percent_decode_path("foo%2"), "foo%2");
        assert_eq!(percent_decode_path("foo%"), "foo%");
    }
}

// ── idle timeout (sliding-window) tests ─────────────────────────────

mod idle_timeout_tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::wait_for_genuine_idle;
    extern crate alloc;

    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicU64, Ordering};

    fn now_epoch() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |pred| pred.as_secs())
    }

    #[tokio::test]
    async fn genuine_idle_returns_promptly() {
        let last_activity = AtomicU64::new(now_epoch());
        // 100 ms timeout, no further activity → should return in ~100 ms.
        let start = tokio::time::Instant::now();
        wait_for_genuine_idle(&last_activity, 1).await;
        // It must have waited at least ~1 s (the timeout).
        // With epoch-second resolution, accept ≥ 900 ms.
        assert!(
            start.elapsed() >= core::time::Duration::from_millis(900),
            "should wait for the full timeout window"
        );
    }

    #[tokio::test]
    async fn activity_extends_deadline() {
        // `wait_for_genuine_idle` works in epoch-second resolution, so
        // we must use a 2 s timeout and poke at ~1 s to ensure the
        // epoch second value actually advances between the initial
        // snapshot and the update.
        let last_activity = Arc::new(AtomicU64::new(now_epoch()));
        let la = Arc::clone(&last_activity);

        // 2 s timeout. At ~1.2 s, poke activity (ensures epoch second
        // advances).
        let updater = tokio::spawn(async move {
            tokio::time::sleep(core::time::Duration::from_millis(1200)).await;
            la.store(now_epoch(), Ordering::Relaxed);
        });

        let start = tokio::time::Instant::now();
        wait_for_genuine_idle(&last_activity, 2).await;
        updater.await.unwrap();

        // The activity at ~1.2 s should push the deadline to ~3.2 s.
        // Total wall time must exceed 2 s (the base timeout).
        assert!(
            start.elapsed() >= core::time::Duration::from_millis(2800),
            "activity at 1.2 s should extend total wait beyond 2 s; actual: {:?}",
            start.elapsed()
        );
    }
}
