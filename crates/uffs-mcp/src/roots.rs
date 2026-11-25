// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! MCP roots mapping policy.
//!
//! When a client advertises roots (workspace directories), this module maps
//! them to UFFS drive/path scope filters so searches are bounded to the
//! workspace.  Unmappable roots (e.g. macOS paths for NTFS capture data)
//! produce warnings rather than silent incorrect scoping.

// SEP-2577 deprecates the MCP Roots capability upstream with NO replacement
// API (workspace context moves to explicit tool inputs). This module is
// deliberately **transport-agnostic**: it never touches `rmcp::model::Root`.
// The one place that still speaks the deprecated wire API is the narrow
// `on_roots_list_changed` hook in `handler/mod.rs` — when rmcp removes the
// API, that hook is deleted and everything here keeps working, fed by
// explicit path filters instead.

extern crate alloc;

use alloc::sync::Arc;

use tokio::sync::RwLock;
use uffs_client::protocol::{
    SearchParams, SearchPredicate, SearchPredicateOp, SearchPredicateValue,
};

/// A single resolved root scope entry.
#[derive(Debug, Clone)]
pub struct RootScope {
    /// Original URI from the client (e.g. `"file:///C:/Users/me/project"`).
    pub uri: String,
    /// Display name from the client, if any.
    pub name: Option<String>,
    /// Normalized NTFS path prefix (e.g. `"C:\\Users\\me\\project"`).
    /// `None` if the root could not be mapped to an NTFS path.
    pub ntfs_prefix: Option<String>,
    /// NTFS drive letter, upper-case (e.g. `'C'`).
    /// `None` if unmappable.
    pub drive_letter: Option<char>,
}

/// A workspace root as advertised by the client, decoupled from the
/// transport: `rmcp::model::Root` is SEP-2577-deprecated, so only the
/// `handler` boundary hook converts from it — this module and its tests
/// never name the deprecated type.
#[derive(Debug, Clone)]
pub(crate) struct AdvertisedRoot {
    /// Root URI as sent by the client (e.g. `"file:///C:/Users/me/project"`).
    pub(crate) uri: String,
    /// Optional display name from the client.
    pub(crate) name: Option<String>,
}

/// Shared roots state held by the MCP server.
///
/// `Clone` is derived so the MCP handler can snapshot the current
/// state under a brief read guard and then release the lock before
/// dispatching tools that await on the daemon RPC.  Cloning is cheap:
/// the `roots` and `warnings` `Vec`s are typically `< 10` short
/// `String`s each (one per workspace root).  See Phase 10b audit
/// findings and `crate::handler::UffsMcpServer::call_tool_with_args`.
#[derive(Debug, Default, Clone)]
pub struct RootsState {
    /// Whether the client has advertised roots at least once.
    pub advertised: bool,
    /// Resolved root scopes.
    pub roots: Vec<RootScope>,
    /// Warnings for roots that could not be mapped to NTFS paths.
    pub warnings: Vec<String>,
}

/// Thread-safe handle to the roots state.
pub(crate) type SharedRootsState = Arc<RwLock<RootsState>>;

/// Parse a `file://` URI into an NTFS-style path.
///
/// Handles:
/// - `file:///C:/path/to/dir`   → `Some("C:\\path\\to\\dir")`
/// - `file:///C%3A/path`        → `Some("C:\\path")`
/// - `file:///home/user/dir`    → `None` (not a Windows drive path)
/// - `https://...`              → `None`
fn parse_file_uri_to_ntfs_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file:///")?;

    // Percent-decode the colon after the drive letter.
    let decoded = rest.replace("%3A", ":").replace("%3a", ":");

    // Must start with a single ASCII letter followed by `:` or `:/`.
    let mut chars = decoded.chars();
    let drive = chars.next()?;
    if !drive.is_ascii_alphabetic() {
        return None;
    }
    let colon = chars.next()?;
    if colon != ':' {
        return None;
    }

    // Normalize to backslashes and ensure no trailing slash.
    let path = decoded.replace('/', "\\");
    let trimmed = path.trim_end_matches('\\');
    Some(trimmed.to_owned())
}

/// Resolve a single [`AdvertisedRoot`] into a [`RootScope`].
fn resolve_root(root: &AdvertisedRoot) -> RootScope {
    let ntfs_prefix = parse_file_uri_to_ntfs_path(&root.uri);
    let drive_letter = ntfs_prefix
        .as_ref()
        .and_then(|path| path.chars().next())
        .map(|ch| ch.to_ascii_uppercase());

    RootScope {
        uri: root.uri.clone(),
        name: root.name.clone(),
        ntfs_prefix,
        drive_letter,
    }
}

/// Update the [`RootsState`] from a list of roots received from the client.
pub(crate) fn update_roots_state(state: &mut RootsState, roots: &[AdvertisedRoot]) {
    state.advertised = true;
    state.roots.clear();
    state.warnings.clear();

    for root in roots {
        let scope = resolve_root(root);
        if scope.ntfs_prefix.is_none() {
            let label = scope.name.as_deref().unwrap_or(scope.uri.as_str());
            state.warnings.push(format!(
                "Root '{label}' could not be mapped to an NTFS drive path and will be ignored \
                 for search scoping. This is expected when UFFS indexes offline NTFS \
                 captures on a non-Windows host.",
            ));
        }
        state.roots.push(scope);
    }
}

/// Collect path predicates from the current roots state.
///
/// Returns a `(drives, path_prefixes, warnings)` tuple:
/// - `drives`: unique drive letters to restrict searches to
/// - `path_prefixes`: normalized NTFS path prefixes for path-within filtering
/// - `warnings`: any warnings about unmappable roots
///
/// Returns `None` if no roots have been advertised or all roots are unmappable.
#[must_use]
pub(crate) fn roots_scope(state: &RootsState) -> Option<(Vec<String>, Vec<String>, Vec<String>)> {
    if !state.advertised || state.roots.is_empty() {
        return None;
    }

    let mappable: Vec<&RootScope> = state
        .roots
        .iter()
        .filter(|root| root.ntfs_prefix.is_some())
        .collect();

    if mappable.is_empty() {
        // All roots unmappable — don't restrict (return warnings only).
        return Some((vec![], vec![], state.warnings.clone()));
    }

    let mut drives: Vec<String> = mappable
        .iter()
        .filter_map(|root| root.drive_letter.map(|ch| ch.to_string()))
        .collect();
    drives.sort_unstable();
    drives.dedup();

    let prefixes: Vec<String> = mappable
        .iter()
        .filter_map(|root| root.ntfs_prefix.clone())
        .collect();

    Some((drives, prefixes, state.warnings.clone()))
}

/// Apply roots-based scoping to a [`SearchParams`].
///
/// When the client has advertised roots and the search params don't already
/// have explicit drive or path predicates, this function:
///
/// 1. Sets `params.drives` to the union of root drive letters.
/// 2. Injects `path starts_with <prefix>` predicates for each root that is
///    narrower than a drive root (e.g. `C:\Users\me\project` but not just
///    `C:`).
///
/// If a root points to a drive root (e.g. `C:`), no path predicate is
/// injected — the drive filter alone is sufficient.
pub(crate) fn apply_roots_scope(state: &RootsState, params: &mut SearchParams) {
    let Some((drives, prefixes, _warnings)) = roots_scope(state) else {
        return;
    };

    // Only apply drive scoping when the caller didn't set explicit drives.
    // Drives advertised by the client come from the `roots/list` MCP
    // exchange; we validate via `DriveLetter::parse` and drop entries
    // that aren't ASCII A..=Z (silently — malformed roots are a client
    // bug, not a server-side error).
    if params.drives.is_empty() && !drives.is_empty() {
        params.drives = drives
            .iter()
            .filter_map(|drv| drv.chars().next())
            .filter_map(|ch| uffs_mft::platform::DriveLetter::parse(ch).ok())
            .collect();
    }

    // Only inject path-prefix predicates when the caller didn't already
    // include any path predicates.
    let has_path_predicate = params
        .predicates
        .iter()
        .any(|pred| pred.field == "path" || pred.field == "path_only");
    if has_path_predicate {
        return;
    }

    // Filter to prefixes that are narrower than a bare drive root
    // (i.e. more than just "X:").
    let narrow_prefixes: Vec<&String> = prefixes
        .iter()
        .filter(|pfx| pfx.len() > 2) // Skip bare "C:" style roots
        .collect();

    if narrow_prefixes.is_empty() {
        return;
    }

    // For a single root, inject a simple StartsWith predicate.
    // For multiple roots, inject an OR-equivalent using HasAny on the
    // prefix list — but since StartsWith only takes a single string,
    // we use one predicate per prefix. The daemon evaluates predicates
    // conjunctively (AND), so multiple StartsWith would be too narrow.
    //
    // For multiple roots we therefore pick the common strategy: inject
    // a single predicate with the *shortest* common prefix, OR if the
    // roots are on different subtrees we skip path filtering entirely
    // and rely on drive filtering alone.
    if let [single] = narrow_prefixes.as_slice() {
        params.predicates.push(SearchPredicate {
            field: "path".to_owned(),
            op: SearchPredicateOp::StartsWith,
            value: SearchPredicateValue::String((*single).clone()),
        });
    } else if let Some(common) = longest_common_prefix(&narrow_prefixes)
        && common.len() > 2
    {
        // Multiple narrow roots share a common prefix that is narrower
        // than a bare drive root — inject it as a single predicate.
        params.predicates.push(SearchPredicate {
            field: "path".to_owned(),
            op: SearchPredicateOp::StartsWith,
            value: SearchPredicateValue::String(common),
        });
    }
    // Otherwise: drive scoping is the best we can do.
}

/// Find the longest common prefix among a set of path strings.
///
/// Returns `None` if the slice is empty, or `Some("")` if there is no
/// common prefix.
fn longest_common_prefix(paths: &[&String]) -> Option<String> {
    let first = paths.first()?;
    let mut prefix_len = first.len();

    for path in paths.get(1..).unwrap_or_default() {
        prefix_len = first
            .chars()
            .zip(path.chars())
            .take(prefix_len)
            .take_while(|(lhs, rhs)| lhs.eq_ignore_ascii_case(rhs))
            .count();
        if prefix_len == 0 {
            return Some(String::new());
        }
    }

    // Truncate to the last path separator to avoid partial directory names.
    // `.get()` avoids `string_slice` / `indexing_slicing` on `&str`.
    let prefix = first.get(..prefix_len).unwrap_or(first.as_str());
    prefix.rfind('\\').map_or_else(
        || Some(prefix.to_owned()),
        |sep_pos| Some(first.get(..=sep_pos).unwrap_or(first.as_str()).to_owned()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_root(uri: &str, name: Option<&str>) -> AdvertisedRoot {
        AdvertisedRoot {
            uri: uri.to_owned(),
            name: name.map(str::to_owned),
        }
    }

    #[test]
    fn parse_windows_file_uri() {
        let path = parse_file_uri_to_ntfs_path("file:///C:/Users/me/project").unwrap();
        assert_eq!(path, r"C:\Users\me\project");
    }

    #[test]
    fn parse_percent_encoded_colon() {
        let path = parse_file_uri_to_ntfs_path("file:///D%3A/Data/logs").unwrap();
        assert_eq!(path, r"D:\Data\logs");
    }

    #[test]
    fn parse_trailing_slash_stripped() {
        let path = parse_file_uri_to_ntfs_path("file:///E:/Games/").unwrap();
        assert_eq!(path, r"E:\Games");
    }

    #[test]
    fn parse_drive_root() {
        let path = parse_file_uri_to_ntfs_path("file:///C:/").unwrap();
        assert_eq!(path, "C:");
    }

    #[test]
    fn parse_unix_path_returns_none() {
        assert!(parse_file_uri_to_ntfs_path("file:///home/user/project").is_none());
    }

    #[test]
    fn parse_https_returns_none() {
        assert!(parse_file_uri_to_ntfs_path("https://github.com/repo").is_none());
    }

    #[test]
    fn update_roots_state_maps_and_warns() {
        let roots = vec![
            make_root("file:///C:/Users/me/project", Some("My Project")),
            make_root("file:///D:/Data", None),
            make_root("file:///home/user/stuff", Some("Unix Root")),
        ];

        let mut state = RootsState::default();
        update_roots_state(&mut state, &roots);

        assert!(state.advertised);
        assert_eq!(state.roots.len(), 3);
        // First two are mappable
        assert_eq!(
            state
                .roots
                .first()
                .expect("roots has an element 0")
                .drive_letter,
            Some('C')
        );
        assert_eq!(
            state
                .roots
                .first()
                .expect("roots has an element 0")
                .ntfs_prefix
                .as_deref(),
            Some(r"C:\Users\me\project")
        );
        assert_eq!(
            state
                .roots
                .get(1)
                .expect("roots has an element 1")
                .drive_letter,
            Some('D')
        );
        // Third is unmappable
        assert!(
            state
                .roots
                .get(2)
                .expect("roots has an element 2")
                .ntfs_prefix
                .is_none()
        );
        assert_eq!(state.warnings.len(), 1);
        assert!(
            state
                .warnings
                .first()
                .expect("warnings has an element 0")
                .contains("Unix Root")
        );
    }

    #[test]
    fn roots_scope_returns_drives_and_prefixes() {
        let roots = vec![
            make_root("file:///C:/Users/me/project", None),
            make_root("file:///D:/Data", None),
        ];

        let mut state = RootsState::default();
        update_roots_state(&mut state, &roots);

        let (drives, prefixes, warnings) = roots_scope(&state).unwrap();
        assert_eq!(drives, vec!["C", "D"]);
        assert_eq!(prefixes.len(), 2);
        assert_eq!(warnings, Vec::<String>::new());
    }

    #[test]
    fn roots_scope_none_when_not_advertised() {
        let state = RootsState::default();
        assert!(roots_scope(&state).is_none());
    }

    #[test]
    fn roots_scope_all_unmappable_returns_empty_drives() {
        let roots = vec![make_root("file:///home/user/stuff", None)];
        let mut state = RootsState::default();
        update_roots_state(&mut state, &roots);

        let (drives, prefixes, warnings) = roots_scope(&state).unwrap();
        assert_eq!(drives, Vec::<String>::new());
        assert_eq!(prefixes, Vec::<String>::new());
        assert_eq!(warnings.len(), 1);
    }

    // ── apply_roots_scope tests ─────────────────────────────────────

    #[test]
    fn apply_roots_scope_single_narrow_root_injects_predicate() {
        let roots = vec![make_root("file:///C:/Users/me/project", None)];
        let mut state = RootsState::default();
        update_roots_state(&mut state, &roots);

        let mut params = SearchParams::default();
        apply_roots_scope(&state, &mut params);

        assert_eq!(params.drives, vec![uffs_mft::platform::DriveLetter::C]);
        assert_eq!(params.predicates.len(), 1);
        assert_eq!(
            params
                .predicates
                .first()
                .expect("predicates has an element 0")
                .field,
            "path"
        );
        assert_eq!(
            params
                .predicates
                .first()
                .expect("predicates has an element 0")
                .op,
            SearchPredicateOp::StartsWith
        );
        assert_eq!(
            params
                .predicates
                .first()
                .expect("predicates has an element 0")
                .value,
            SearchPredicateValue::String(r"C:\Users\me\project".to_owned())
        );
    }

    #[test]
    fn apply_roots_scope_drive_root_no_path_predicate() {
        let roots = vec![make_root("file:///D:/", None)];
        let mut state = RootsState::default();
        update_roots_state(&mut state, &roots);

        let mut params = SearchParams::default();
        apply_roots_scope(&state, &mut params);

        assert_eq!(params.drives, vec![uffs_mft::platform::DriveLetter::D]);
        // "D:" is only 2 chars — should NOT inject a path predicate.
        assert_eq!(params.predicates, Vec::<SearchPredicate>::new());
    }

    #[test]
    fn apply_roots_scope_respects_explicit_drives() {
        let roots = vec![make_root("file:///C:/Users/me/project", None)];
        let mut state = RootsState::default();
        update_roots_state(&mut state, &roots);

        let mut params = SearchParams {
            drives: vec![uffs_mft::platform::DriveLetter::D], // User explicitly set drives.
            ..Default::default()
        };
        apply_roots_scope(&state, &mut params);

        // Should NOT override explicit drives.
        assert_eq!(params.drives, vec![uffs_mft::platform::DriveLetter::D]);
        // But should still add path predicate since no path pred exists.
        assert_eq!(params.predicates.len(), 1);
    }

    #[test]
    fn apply_roots_scope_skips_when_path_predicate_exists() {
        let roots = vec![make_root("file:///C:/Users/me/project", None)];
        let mut state = RootsState::default();
        update_roots_state(&mut state, &roots);

        let mut params = SearchParams {
            predicates: vec![SearchPredicate {
                field: "path".to_owned(),
                op: SearchPredicateOp::Contains,
                value: SearchPredicateValue::String("foo".to_owned()),
            }],
            ..Default::default()
        };
        apply_roots_scope(&state, &mut params);

        // Should NOT add another path predicate.
        assert_eq!(params.predicates.len(), 1);
        assert_eq!(
            params
                .predicates
                .first()
                .expect("predicates has an element 0")
                .op,
            SearchPredicateOp::Contains
        );
    }

    #[test]
    fn apply_roots_scope_multiple_roots_common_prefix() {
        let roots = vec![
            make_root("file:///C:/Users/me/project-a", None),
            make_root("file:///C:/Users/me/project-b", None),
        ];
        let mut state = RootsState::default();
        update_roots_state(&mut state, &roots);

        let mut params = SearchParams::default();
        apply_roots_scope(&state, &mut params);

        assert_eq!(params.drives, vec![uffs_mft::platform::DriveLetter::C]);
        // Common prefix is "C:\Users\me\" (truncated at last separator).
        assert_eq!(params.predicates.len(), 1);
        assert_eq!(
            params
                .predicates
                .first()
                .expect("predicates has an element 0")
                .value,
            SearchPredicateValue::String(r"C:\Users\me\".to_owned())
        );
    }

    #[test]
    fn apply_roots_scope_no_roots_is_noop() {
        let state = RootsState::default();
        let mut params = SearchParams::default();
        apply_roots_scope(&state, &mut params);

        assert_eq!(params.drives, Vec::<uffs_mft::platform::DriveLetter>::new());
        assert_eq!(params.predicates, Vec::<SearchPredicate>::new());
    }

    // ── longest_common_prefix tests ─────────────────────────────────

    #[test]
    fn lcp_identical_paths() {
        let paths = ["foo\\bar".to_owned(), "foo\\bar".to_owned()];
        let refs: Vec<&String> = paths.iter().collect();
        // Full match → truncates to last separator.
        assert_eq!(longest_common_prefix(&refs), Some("foo\\".to_owned()));
    }

    #[test]
    fn lcp_divergent_paths() {
        let paths = ["C:\\Alpha\\x".to_owned(), "C:\\Beta\\y".to_owned()];
        let refs: Vec<&String> = paths.iter().collect();
        assert_eq!(longest_common_prefix(&refs), Some("C:\\".to_owned()));
    }

    #[test]
    fn lcp_no_common() {
        let paths = ["A:\\x".to_owned(), "B:\\y".to_owned()];
        let refs: Vec<&String> = paths.iter().collect();
        assert_eq!(longest_common_prefix(&refs), Some(String::new()));
    }

    #[test]
    fn lcp_empty_slice() {
        let refs: Vec<&String> = vec![];
        assert_eq!(longest_common_prefix(&refs), None);
    }
}
