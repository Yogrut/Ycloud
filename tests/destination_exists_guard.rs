//! Guards the typed "destination exists" condition against regressions.
//!
//! Batch keep-both retries only on `AppError::DestinationExists`. If a new call
//! site writes the old message-based conflict instead, the retry silently stops
//! working, so this test fails on the string form anywhere in non-test source.

use std::path::{Path, PathBuf};

fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, files);
        } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
            files.push(path);
        }
    }
}

#[test]
fn destination_conflicts_use_the_typed_variant() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect(&root, &mut files);

    let needles = [
        "Conflict(\"Destination already exists\"",
        "Conflict(\"目标路径已经存在\"",
        "Conflict(\"目标对象已存在\"",
    ];
    let mut offenders = Vec::new();
    for path in files {
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if name == "tests.rs" || name.ends_with("_tests.rs") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for needle in needles {
            if text.contains(needle) {
                offenders.push(format!("{}: {needle}", path.display()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "use AppError::DestinationExists instead of a message conflict: {offenders:?}"
    );
}
