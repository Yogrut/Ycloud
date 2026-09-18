use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendEntry {
    pub name: String,
    pub relative: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified_unix: Option<i64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectorySort {
    #[default]
    Name,
    Size,
    Time,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDirection {
    #[default]
    Asc,
    Desc,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EntryPosition {
    is_dir: bool,
    name: String,
    relative: String,
    size: u64,
    modified_unix: Option<i64>,
}

impl From<&BackendEntry> for EntryPosition {
    fn from(entry: &BackendEntry) -> Self {
        Self {
            is_dir: entry.is_dir,
            name: entry.name.clone(),
            relative: entry.relative.clone(),
            size: entry.size,
            modified_unix: entry.modified_unix,
        }
    }
}

impl EntryPosition {
    pub(crate) fn as_entry(&self) -> BackendEntry {
        BackendEntry {
            name: self.name.clone(),
            relative: self.relative.clone(),
            is_dir: self.is_dir,
            size: self.size,
            modified_unix: self.modified_unix,
        }
    }
}

pub(crate) fn normalized_search(search: Option<&str>) -> Option<String> {
    search
        .map(str::trim)
        .map(str::to_lowercase)
        .filter(|value| !value.is_empty())
}

pub(crate) fn prepare_snapshot(
    entries: Vec<BackendEntry>,
    search: Option<&str>,
    sort: DirectorySort,
    direction: SortDirection,
) -> Vec<BackendEntry> {
    let search = normalized_search(search);
    let mut prepared = entries
        .into_iter()
        .filter_map(|entry| {
            let normalized_name = entry.name.to_lowercase();
            search
                .as_ref()
                .is_none_or(|search| normalized_name.contains(search))
                .then_some((normalized_name, entry))
        })
        .collect::<Vec<_>>();
    prepared.sort_unstable_by(|(left_name, left), (right_name, right)| {
        compare_entries_with_names(left, left_name, right, right_name, sort, direction)
    });
    prepared.into_iter().map(|(_, entry)| entry).collect()
}

pub(crate) fn page_from_snapshot(
    entries: &[BackendEntry],
    request: &DirectoryListRequest,
) -> DirectoryPage {
    let limit = request.limit.max(1);
    let start = request.after.as_ref().map_or(0, |after| {
        let after = after.as_entry();
        entries.partition_point(|entry| {
            compare_entries(entry, &after, request.sort, request.direction) != Ordering::Greater
        })
    });
    let end = start.saturating_add(limit).min(entries.len());
    let page = entries[start..end].to_vec();
    let next_position = (end < entries.len())
        .then(|| page.last().map(EntryPosition::from))
        .flatten();
    DirectoryPage {
        entries: page,
        next_position,
    }
}

#[derive(Clone, Debug)]
pub struct DirectoryListRequest {
    pub limit: usize,
    pub search: Option<String>,
    pub sort: DirectorySort,
    pub direction: SortDirection,
    pub after: Option<EntryPosition>,
}

#[derive(Debug)]
pub struct DirectoryPage {
    pub entries: Vec<BackendEntry>,
    pub next_position: Option<EntryPosition>,
}

/// Keeps only one sorted page while a backend streams an arbitrarily large
/// directory. This avoids making the HTTP layer materialize the full listing.
pub struct DirectoryPageCollector {
    request: DirectoryListRequest,
    entries: Vec<BackendEntry>,
}

impl DirectoryPageCollector {
    pub fn new(mut request: DirectoryListRequest) -> Self {
        request.limit = request.limit.max(1);
        request.search = normalized_search(request.search.as_deref());
        Self {
            entries: Vec::with_capacity(request.limit.saturating_add(1)),
            request,
        }
    }

    pub fn consider(&mut self, entry: BackendEntry) {
        if self
            .request
            .search
            .as_ref()
            .is_some_and(|search| !entry.name.to_lowercase().contains(search))
        {
            return;
        }
        if self.request.after.as_ref().is_some_and(|after| {
            compare_entries(
                &entry,
                &after.as_entry(),
                self.request.sort,
                self.request.direction,
            ) != Ordering::Greater
        }) {
            return;
        }

        let insertion = self
            .entries
            .binary_search_by(|candidate| {
                compare_entries(candidate, &entry, self.request.sort, self.request.direction)
            })
            .unwrap_or_else(|index| index);
        self.entries.insert(insertion, entry);
        if self.entries.len() > self.request.limit.saturating_add(1) {
            self.entries.pop();
        }
    }

    pub fn finish(mut self) -> DirectoryPage {
        let has_more = self.entries.len() > self.request.limit;
        self.entries.truncate(self.request.limit);
        let next_position = has_more
            .then(|| self.entries.last().map(EntryPosition::from))
            .flatten();
        DirectoryPage {
            entries: self.entries,
            next_position,
        }
    }
}

pub fn compare_entries(
    left: &BackendEntry,
    right: &BackendEntry,
    sort: DirectorySort,
    direction: SortDirection,
) -> Ordering {
    compare_entries_with_names(
        left,
        &normalized_name(left),
        right,
        &normalized_name(right),
        sort,
        direction,
    )
}

fn compare_entries_with_names(
    left: &BackendEntry,
    left_name: &str,
    right: &BackendEntry,
    right_name: &str,
    sort: DirectorySort,
    direction: SortDirection,
) -> Ordering {
    let kind_order = right.is_dir.cmp(&left.is_dir);
    if kind_order != Ordering::Equal {
        return kind_order;
    }
    let primary = match sort {
        DirectorySort::Name => left_name.cmp(right_name),
        DirectorySort::Size => left.size.cmp(&right.size),
        DirectorySort::Time => left.modified_unix.cmp(&right.modified_unix),
    };
    let primary = match direction {
        SortDirection::Asc => primary,
        SortDirection::Desc => primary.reverse(),
    };
    primary.then_with(|| {
        left_name
            .cmp(right_name)
            .then(left.relative.cmp(&right.relative))
    })
}

fn normalized_name(entry: &BackendEntry) -> String {
    entry.name.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{
        page_from_snapshot, prepare_snapshot, BackendEntry, DirectoryListRequest,
        DirectoryPageCollector, DirectorySort, SortDirection,
    };

    fn entry(name: &str, size: u64) -> BackendEntry {
        BackendEntry {
            name: name.into(),
            relative: name.into(),
            is_dir: false,
            size,
            modified_unix: Some(size as i64),
        }
    }

    #[test]
    fn collector_pages_without_materializing_the_full_directory() {
        let mut first = DirectoryPageCollector::new(DirectoryListRequest {
            limit: 2,
            search: None,
            sort: DirectorySort::Name,
            direction: SortDirection::Asc,
            after: None,
        });
        for value in [entry("d", 4), entry("a", 1), entry("c", 3), entry("b", 2)] {
            first.consider(value);
        }
        let first = first.finish();
        assert_eq!(
            first
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );

        let mut second = DirectoryPageCollector::new(DirectoryListRequest {
            limit: 2,
            search: None,
            sort: DirectorySort::Name,
            direction: SortDirection::Asc,
            after: first.next_position,
        });
        for value in [entry("d", 4), entry("a", 1), entry("c", 3), entry("b", 2)] {
            second.consider(value);
        }
        let second = second.finish();
        assert_eq!(
            second
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["c", "d"]
        );
        assert!(second.next_position.is_none());
    }

    #[test]
    fn collector_applies_search_before_pagination() {
        let mut collector = DirectoryPageCollector::new(DirectoryListRequest {
            limit: 10,
            search: Some("LOG".into()),
            sort: DirectorySort::Size,
            direction: SortDirection::Desc,
            after: None,
        });
        for value in [
            entry("readme.md", 10),
            entry("app.log", 20),
            entry("old.LOG", 5),
        ] {
            collector.consider(value);
        }
        assert_eq!(
            collector
                .finish()
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["app.log", "old.LOG"]
        );
    }

    #[test]
    #[ignore = "manual D1 performance baseline"]
    fn repeated_page_scan_performance_baseline() {
        for count in [1_000_usize, 10_000, 100_000] {
            for sort in [
                DirectorySort::Name,
                DirectorySort::Size,
                DirectorySort::Time,
            ] {
                let started = std::time::Instant::now();
                let mut after = None;
                let mut first_page_elapsed = std::time::Duration::ZERO;
                for page in 0..4 {
                    let page_started = std::time::Instant::now();
                    let mut collector = DirectoryPageCollector::new(DirectoryListRequest {
                        limit: 100,
                        search: Some("0".into()),
                        sort,
                        direction: SortDirection::Asc,
                        after,
                    });
                    for index in 0..count {
                        let value = index.wrapping_mul(48_271) % count;
                        collector.consider(BackendEntry {
                            name: format!("entry-{value:06}.dat"),
                            relative: format!("entry-{value:06}.dat"),
                            is_dir: value % 11 == 0,
                            size: (value as u64).wrapping_mul(65_537) % 10_000_000,
                            modified_unix: Some(1_700_000_000 + value as i64),
                        });
                    }
                    let result = std::hint::black_box(collector.finish());
                    assert_eq!(result.entries.len(), 100);
                    after = result.next_position;
                    if page == 0 {
                        first_page_elapsed = page_started.elapsed();
                    }
                }
                eprintln!(
                    "directory_baseline entries={count} sort={sort:?} first_page={first_page_elapsed:?} four_pages={:?}",
                    started.elapsed()
                );

                let build_started = std::time::Instant::now();
                let snapshot = prepare_snapshot(
                    (0..count)
                        .map(|index| {
                            let value = index.wrapping_mul(48_271) % count;
                            BackendEntry {
                                name: format!("entry-{value:06}.dat"),
                                relative: format!("entry-{value:06}.dat"),
                                is_dir: value % 11 == 0,
                                size: (value as u64).wrapping_mul(65_537) % 10_000_000,
                                modified_unix: Some(1_700_000_000 + value as i64),
                            }
                        })
                        .collect(),
                    Some("0"),
                    sort,
                    SortDirection::Asc,
                );
                let build_elapsed = build_started.elapsed();
                let page_started = std::time::Instant::now();
                let mut after = None;
                for _ in 0..4 {
                    let result = page_from_snapshot(
                        &snapshot,
                        &DirectoryListRequest {
                            limit: 100,
                            search: Some("0".into()),
                            sort,
                            direction: SortDirection::Asc,
                            after,
                        },
                    );
                    after = result.next_position;
                    std::hint::black_box(result.entries);
                }
                eprintln!(
                    "directory_snapshot entries={count} sort={sort:?} build={build_elapsed:?} four_pages={:?}",
                    page_started.elapsed()
                );
            }
        }
    }
}
