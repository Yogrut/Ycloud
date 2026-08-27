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

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectorySort {
    #[default]
    Name,
    Size,
    Time,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
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
    fn as_entry(&self) -> BackendEntry {
        BackendEntry {
            name: self.name.clone(),
            relative: self.relative.clone(),
            is_dir: self.is_dir,
            size: self.size,
            modified_unix: self.modified_unix,
        }
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
        request.search = request
            .search
            .take()
            .map(|value| value.trim().to_lowercase())
            .filter(|value| !value.is_empty());
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
    let kind_order = right.is_dir.cmp(&left.is_dir);
    if kind_order != Ordering::Equal {
        return kind_order;
    }
    let primary = match sort {
        DirectorySort::Name => normalized_name(left).cmp(&normalized_name(right)),
        DirectorySort::Size => left.size.cmp(&right.size),
        DirectorySort::Time => left.modified_unix.cmp(&right.modified_unix),
    };
    let primary = match direction {
        SortDirection::Asc => primary,
        SortDirection::Desc => primary.reverse(),
    };
    primary.then_with(|| {
        normalized_name(left)
            .cmp(&normalized_name(right))
            .then(left.relative.cmp(&right.relative))
    })
}

fn normalized_name(entry: &BackendEntry) -> String {
    entry.name.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{
        BackendEntry, DirectoryListRequest, DirectoryPageCollector, DirectorySort, SortDirection,
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
}
