//! Retry policy tests for batch keep-both, driven by fake closures.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use super::keep_both_with_retries;
use crate::error::AppError;

fn candidates(names: &[&str]) -> Arc<Mutex<VecDeque<String>>> {
    let owned: VecDeque<String> = names.iter().map(|name| name.to_string()).collect();
    Arc::new(Mutex::new(owned))
}

#[tokio::test]
async fn retries_until_a_candidate_is_free() {
    let names = candidates(&["a1", "a2", "a3"]);
    let taken = Arc::new(Mutex::new(Vec::new()));
    let seen_candidates = Arc::new(Mutex::new(Vec::new()));
    let (n, t, s) = (names.clone(), taken.clone(), seen_candidates.clone());
    let result = keep_both_with_retries(
        3,
        move |reserved: HashSet<String>| {
            let names = n.clone();
            let seen = s.clone();
            Box::pin(async move {
                seen.lock().unwrap().push(reserved.len());
                Ok(names.lock().unwrap().pop_front().unwrap())
            })
        },
        move |candidate: String| {
            let taken = t.clone();
            Box::pin(async move {
                let mut taken = taken.lock().unwrap();
                taken.push(candidate);
                if taken.len() < 3 {
                    Err(AppError::DestinationExists)
                } else {
                    Ok(())
                }
            })
        },
    )
    .await;
    assert!(result.is_ok());
    assert_eq!(*taken.lock().unwrap(), ["a1", "a2", "a3"]);
    // The reserved set grows once per collision, so later candidates are fresh.
    assert_eq!(*seen_candidates.lock().unwrap(), [0, 1, 2]);
}

#[tokio::test]
async fn reports_how_many_names_were_tried() {
    let names = candidates(&["a1", "a2", "a3"]);
    let n = names.clone();
    let result = keep_both_with_retries(
        3,
        move |_reserved: HashSet<String>| {
            let names = n.clone();
            Box::pin(async move { Ok(names.lock().unwrap().pop_front().unwrap()) })
        },
        move |_candidate: String| Box::pin(async { Err(AppError::DestinationExists) }),
    )
    .await;
    match result {
        Err(AppError::Conflict(message)) => assert!(message.contains("已尝试 3 个编号")),
        other => panic!("expected a three-attempt conflict, got {other:?}"),
    }
}

#[tokio::test]
async fn other_errors_are_not_retried() {
    let names = candidates(&["a1", "a2", "a3"]);
    let attempts = Arc::new(Mutex::new(0_usize));
    let (n, a) = (names.clone(), attempts.clone());
    let result = keep_both_with_retries(
        3,
        move |_reserved: HashSet<String>| {
            let names = n.clone();
            Box::pin(async move { Ok(names.lock().unwrap().pop_front().unwrap()) })
        },
        move |_candidate: String| {
            let attempts = a.clone();
            Box::pin(async move {
                *attempts.lock().unwrap() += 1;
                Err(AppError::Conflict("denied".into()))
            })
        },
    )
    .await;
    assert!(result.is_err());
    assert_eq!(*attempts.lock().unwrap(), 1);
}

#[tokio::test]
async fn a_single_attempt_returns_the_original_collision() {
    let names = candidates(&["dir"]);
    let attempts = Arc::new(Mutex::new(0_usize));
    let (n, a) = (names.clone(), attempts.clone());
    let result = keep_both_with_retries(
        1,
        move |_reserved: HashSet<String>| {
            let names = n.clone();
            Box::pin(async move { Ok(names.lock().unwrap().pop_front().unwrap()) })
        },
        move |_candidate: String| {
            let attempts = a.clone();
            Box::pin(async move {
                *attempts.lock().unwrap() += 1;
                Err(AppError::DestinationExists)
            })
        },
    )
    .await;
    assert!(matches!(result, Err(AppError::DestinationExists)));
    assert_eq!(*attempts.lock().unwrap(), 1);
}
