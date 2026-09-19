use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn temp_dir(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("shellcomp-{label}-{unique}"));
    std::fs::create_dir_all(&path).expect("temp dir should be creatable");
    path
}

pub(crate) fn assert_structural_failure(
    error: crate::Error,
    context: &str,
) -> crate::FailureReport {
    assert!(
        matches!(error, crate::Error::Failure { .. }),
        "{context}: expected structured failure, got {error:?}"
    );
    error
        .as_failure()
        .expect("expected structured failure")
        .clone()
}
