#[cfg(test)]
use std::fs;
use std::ops::Range;
use std::path::Path;
use std::sync::OnceLock;

use super::{fs::write_atomic, locks::PathLocks, paths::validate_target_path};
use crate::error::{Error, Result};
use crate::model::FileChange;

// Completion targets and startup files are separate lock domains. Different programs
// share startup files, so their entire read/modify/write transaction must be serialized.
static PROFILE_LOCKS: OnceLock<PathLocks> = OnceLock::new();

#[derive(Debug, Clone)]
pub(crate) struct ManagedBlock {
    pub(crate) start_marker: String,
    pub(crate) end_marker: String,
    pub(crate) body: String,
}

impl ManagedBlock {
    pub(crate) fn render(&self) -> String {
        format!(
            "{}\n{}\n{}\n",
            self.start_marker,
            self.body.trim_end(),
            self.end_marker
        )
    }
}

fn with_profile_lock<T>(path: &Path, run: impl FnOnce() -> Result<T>) -> Result<T> {
    PROFILE_LOCKS
        .get_or_init(PathLocks::default)
        .with_lock(path, || {
            validate_target_path(path)?;
            run()
        })
}

pub(crate) fn upsert(path: &Path, block: &ManagedBlock) -> Result<FileChange> {
    with_profile_lock(path, || {
        let original = read_utf8_file(path)?;
        let updated = rewrite_upsert(path, original.as_deref().unwrap_or_default(), block)?;
        if original.as_deref() == Some(&updated) {
            return Ok(FileChange::Unchanged);
        }
        write_atomic(path, updated.as_bytes())?;
        Ok(if original.is_some() {
            FileChange::Updated
        } else {
            FileChange::Created
        })
    })
}

pub(crate) fn remove(path: &Path, block: &ManagedBlock) -> Result<FileChange> {
    remove_all(path, std::slice::from_ref(block))
}

pub(crate) fn migrate_blocks(
    path: &Path,
    legacy_blocks: &[ManagedBlock],
    managed_block: &ManagedBlock,
) -> Result<(FileChange, FileChange)> {
    with_profile_lock(path, || {
        let original = read_utf8_file(path)?;
        let contents = original.as_deref().unwrap_or_default();
        // Validate all marker pairs before removing anything, including the current block.
        let mut all_blocks = legacy_blocks.to_vec();
        all_blocks.push(managed_block.clone());
        block_ranges_for_all(path, contents, &all_blocks)?;
        let (without_legacy, legacy_change) = rewrite_remove_all(path, contents, legacy_blocks)?;
        let existed = !block_ranges(path, &without_legacy, managed_block)?.is_empty();
        let updated = rewrite_upsert(path, &without_legacy, managed_block)?;
        let managed_change = if without_legacy == updated {
            FileChange::Unchanged
        } else if existed {
            FileChange::Updated
        } else {
            FileChange::Created
        };
        if original.as_deref() != Some(&updated) {
            write_atomic(path, updated.as_bytes())?;
        }
        Ok((legacy_change, managed_change))
    })
}

pub(crate) fn remove_all(path: &Path, blocks: &[ManagedBlock]) -> Result<FileChange> {
    with_profile_lock(path, || {
        let original = read_utf8_file(path)?;
        let (updated, change) =
            rewrite_remove_all(path, original.as_deref().unwrap_or_default(), blocks)?;
        if change == FileChange::Removed {
            write_atomic(path, updated.as_bytes())?;
        }
        Ok(change)
    })
}

pub(crate) fn matches(path: &Path, block: &ManagedBlock) -> Result<bool> {
    with_profile_lock(path, || {
        let contents = read_utf8_file(path)?.unwrap_or_default();
        let ranges = block_ranges(path, &contents, block)?;
        Ok(ranges.len() == 1
            && contents[ranges[0].clone()]
                .lines()
                .eq(block.render().lines()))
    })
}

fn read_utf8_file(path: &Path) -> Result<Option<String>> {
    super::fs::read_file_if_exists(path)?
        .map(|contents| {
            String::from_utf8(contents).map_err(|_| Error::InvalidUtf8File {
                path: path.to_path_buf(),
            })
        })
        .transpose()
}

fn invalid_block(path: &Path, reason: &'static str) -> Error {
    Error::InvalidManagedBlock {
        path: path.to_path_buf(),
        reason,
    }
}

/// Markers occupy complete lines; matching arbitrary substrings could delete user code.
fn block_ranges(path: &Path, contents: &str, block: &ManagedBlock) -> Result<Vec<Range<usize>>> {
    for marker in [&block.start_marker, &block.end_marker] {
        if marker.trim().is_empty() || marker.contains(['\n', '\r']) {
            return Err(invalid_block(path, "markers must be nonempty single lines"));
        }
    }
    if block.start_marker == block.end_marker {
        return Err(invalid_block(
            path,
            "start and end markers must be distinct",
        ));
    }
    let mut ranges = Vec::new();
    let mut start = None;
    let mut offset = 0;
    for line in contents.split_inclusive('\n') {
        let text = line.strip_suffix('\n').unwrap_or(line);
        let text = text.strip_suffix('\r').unwrap_or(text);
        if text == block.start_marker {
            if start.replace(offset).is_some() {
                return Err(invalid_block(path, "managed blocks must not be nested"));
            }
        } else if text == block.end_marker {
            let begin = start
                .take()
                .ok_or_else(|| invalid_block(path, "end marker has no matching start marker"))?;
            ranges.push(begin..offset + line.len());
        }
        offset += line.len();
    }
    if start.is_some() {
        return Err(Error::ManagedBlockMissingEnd {
            path: path.to_path_buf(),
            start_marker: block.start_marker.clone(),
            end_marker: block.end_marker.clone(),
        });
    }
    Ok(ranges)
}

fn block_ranges_for_all(
    path: &Path,
    contents: &str,
    blocks: &[ManagedBlock],
) -> Result<Vec<Range<usize>>> {
    let mut ranges = Vec::new();
    for block in blocks {
        ranges.extend(block_ranges(path, contents, block)?);
    }
    ranges.sort_unstable_by_key(|range| (range.start, range.end));
    ranges.dedup();
    if ranges.windows(2).any(|pair| pair[0].end > pair[1].start) {
        return Err(invalid_block(path, "managed blocks must not overlap"));
    }
    Ok(ranges)
}

fn rewrite_remove_all(
    path: &Path,
    contents: &str,
    blocks: &[ManagedBlock],
) -> Result<(String, FileChange)> {
    let ranges = block_ranges_for_all(path, contents, blocks)?;
    let change = if ranges.is_empty() {
        FileChange::Absent
    } else {
        FileChange::Removed
    };
    Ok((replace_ranges(contents, &ranges, ""), change))
}

fn rewrite_upsert(path: &Path, contents: &str, block: &ManagedBlock) -> Result<String> {
    let ranges = block_ranges(path, contents, block)?;
    let newline = if contents.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let rendered = block.render().replace('\n', newline);
    if ranges.is_empty() {
        let mut updated = contents.to_owned();
        if !updated.is_empty() {
            if !updated.ends_with('\n') {
                updated.push_str(newline);
            }
            updated.push_str(newline);
        }
        updated.push_str(&rendered);
        Ok(updated)
    } else {
        Ok(replace_ranges(contents, &ranges, &rendered))
    }
}

fn replace_ranges(contents: &str, ranges: &[Range<usize>], replacement: &str) -> String {
    let mut updated = String::with_capacity(contents.len() + replacement.len());
    let mut cursor = 0;
    for (index, range) in ranges.iter().enumerate() {
        updated.push_str(&contents[cursor..range.start]);
        if index == 0 {
            updated.push_str(replacement);
        }
        cursor = range.end;
    }
    updated.push_str(&contents[cursor..]);
    updated
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{ManagedBlock, matches, migrate_blocks, remove, remove_all, upsert};
    use crate::model::FileChange;

    #[test]
    fn upsert_is_idempotent() {
        let temp_root = crate::tests::temp_dir("managed-block-upsert");
        let profile = temp_root.join(".shellrc");
        let block = ManagedBlock {
            start_marker: "# >>> shellcomp bash tool >>>".to_owned(),
            end_marker: "# <<< shellcomp bash tool <<<".to_owned(),
            body: "source '/tmp/tool'".to_owned(),
        };

        let first = upsert(&profile, &block).expect("first upsert should succeed");
        let second = upsert(&profile, &block).expect("second upsert should succeed");

        assert_eq!(first, FileChange::Created);
        assert_eq!(second, FileChange::Unchanged);
    }

    #[test]
    fn remove_deletes_all_duplicate_blocks() {
        let temp_root = crate::tests::temp_dir("managed-block-remove");
        let profile = temp_root.join(".shellrc");
        let block = ManagedBlock {
            start_marker: "# >>> shellcomp bash tool >>>".to_owned(),
            end_marker: "# <<< shellcomp bash tool <<<".to_owned(),
            body: "source '/tmp/tool'".to_owned(),
        };

        let duplicate = format!(
            "{}{}\n{}\n{}\n{}\n",
            block.render(),
            "echo keep",
            block.start_marker,
            block.body,
            block.end_marker
        );
        fs::write(&profile, duplicate).expect("profile should be writable");

        let change = remove(&profile, &block).expect("remove should succeed");

        assert_eq!(change, FileChange::Removed);
        let remaining = fs::read_to_string(profile).expect("profile should remain readable");
        assert!(!remaining.contains(&block.start_marker));
        assert!(!remaining.contains(&block.end_marker));
        assert!(remaining.contains("echo keep"));
    }

    #[test]
    fn upsert_replaces_stale_managed_block_body() {
        let temp_root = crate::tests::temp_dir("managed-block-update");
        let profile = temp_root.join(".shellrc");
        let stale = ManagedBlock {
            start_marker: "# >>> shellcomp bash tool >>>".to_owned(),
            end_marker: "# <<< shellcomp bash tool <<<".to_owned(),
            body: "source '/tmp/old-tool'".to_owned(),
        };
        let fresh = ManagedBlock {
            start_marker: stale.start_marker.clone(),
            end_marker: stale.end_marker.clone(),
            body: "source '/tmp/new-tool'".to_owned(),
        };

        upsert(&profile, &stale).expect("stale block should be written");
        let change = upsert(&profile, &fresh).expect("fresh block should be written");

        assert_eq!(change, FileChange::Updated);
        let rendered = fs::read_to_string(profile).expect("profile should remain readable");
        assert!(rendered.contains("/tmp/new-tool"));
        assert!(!rendered.contains("/tmp/old-tool"));
    }

    #[test]
    fn upsert_preserves_existing_block_position() {
        let temp_root = crate::tests::temp_dir("managed-block-position");
        let profile = temp_root.join(".shellrc");
        let stale = ManagedBlock {
            start_marker: "# >>> shellcomp bash tool >>>".to_owned(),
            end_marker: "# <<< shellcomp bash tool <<<".to_owned(),
            body: "source '/tmp/old-tool'".to_owned(),
        };
        let fresh = ManagedBlock {
            start_marker: stale.start_marker.clone(),
            end_marker: stale.end_marker.clone(),
            body: "source '/tmp/new-tool'".to_owned(),
        };
        let contents = format!("export A=1\n{}\necho tail\n", stale.render());
        fs::write(&profile, contents).expect("profile should be writable");

        upsert(&profile, &fresh).expect("upsert should succeed");

        let rendered = fs::read_to_string(profile).expect("profile should remain readable");
        assert!(rendered.starts_with("export A=1\n# >>> shellcomp bash tool >>>"));
        assert!(rendered.contains("echo tail"));
    }

    #[test]
    fn matches_rejects_stale_block_body() {
        let temp_root = crate::tests::temp_dir("managed-block-matches");
        let profile = temp_root.join(".shellrc");
        let stale = ManagedBlock {
            start_marker: "# >>> shellcomp bash tool >>>".to_owned(),
            end_marker: "# <<< shellcomp bash tool <<<".to_owned(),
            body: "source '/tmp/old-tool'".to_owned(),
        };
        let fresh = ManagedBlock {
            start_marker: stale.start_marker.clone(),
            end_marker: stale.end_marker.clone(),
            body: "source '/tmp/new-tool'".to_owned(),
        };

        upsert(&profile, &stale).expect("stale block should be written");

        assert!(!matches(&profile, &fresh).expect("match check should succeed"));
        assert!(matches(&profile, &stale).expect("match check should succeed"));
    }

    #[test]
    fn matches_reports_missing_end_marker() {
        let temp_root = crate::tests::temp_dir("managed-block-matches-corrupt");
        let profile = temp_root.join(".shellrc");
        let block = ManagedBlock {
            start_marker: "# >>> shellcomp bash tool >>>".to_owned(),
            end_marker: "# <<< shellcomp bash tool <<<".to_owned(),
            body: "source '/tmp/tool'".to_owned(),
        };
        fs::write(
            &profile,
            "# >>> shellcomp bash tool >>>\nsource '/tmp/tool'\n",
        )
        .expect("profile should be writable");

        let error = matches(&profile, &block).expect_err("matches should fail");

        assert!(matches!(error, crate::Error::ManagedBlockMissingEnd { .. }));
    }

    #[test]
    fn matches_reports_missing_end_marker_even_when_a_valid_duplicate_exists() {
        let temp_root = crate::tests::temp_dir("managed-block-matches-corrupt-duplicate");
        let profile = temp_root.join(".shellrc");
        let block = ManagedBlock {
            start_marker: "# >>> shellcomp bash tool >>>".to_owned(),
            end_marker: "# <<< shellcomp bash tool <<<".to_owned(),
            body: "source '/tmp/tool'".to_owned(),
        };
        fs::write(
            &profile,
            format!(
                "{}# >>> shellcomp bash tool >>>\nsource '/tmp/other'\n",
                block.render()
            ),
        )
        .expect("profile should be writable");

        let error = matches(&profile, &block).expect_err("matches should fail");

        assert!(matches!(error, crate::Error::ManagedBlockMissingEnd { .. }));
    }

    #[test]
    fn matches_rejects_stale_duplicate_even_when_a_valid_block_exists() {
        let temp_root = crate::tests::temp_dir("managed-block-matches-stale-duplicate");
        let profile = temp_root.join(".shellrc");
        let expected = ManagedBlock {
            start_marker: "# >>> shellcomp bash tool >>>".to_owned(),
            end_marker: "# <<< shellcomp bash tool <<<".to_owned(),
            body: "source '/tmp/tool'".to_owned(),
        };
        let stale = ManagedBlock {
            start_marker: expected.start_marker.clone(),
            end_marker: expected.end_marker.clone(),
            body: "source '/tmp/old-tool'".to_owned(),
        };
        fs::write(&profile, format!("{}{}", expected.render(), stale.render()))
            .expect("profile should be writable");

        assert!(!matches(&profile, &expected).expect("match check should succeed"));
    }

    #[test]
    fn matches_rejects_duplicate_matching_blocks() {
        let temp_root = crate::tests::temp_dir("managed-block-matches-duplicate");
        let profile = temp_root.join(".shellrc");
        let block = ManagedBlock {
            start_marker: "# >>> shellcomp bash tool >>>".to_owned(),
            end_marker: "# <<< shellcomp bash tool <<<".to_owned(),
            body: "source '/tmp/tool'".to_owned(),
        };
        fs::write(&profile, format!("{}{}", block.render(), block.render()))
            .expect("profile should be writable");

        assert!(!matches(&profile, &block).expect("match check should succeed"));
    }

    #[test]
    fn remove_all_is_atomic_when_later_block_is_malformed() {
        let temp_root = crate::tests::temp_dir("managed-block-remove-all-atomic");
        let profile = temp_root.join(".shellrc");
        let first = ManagedBlock {
            start_marker: "# >>> legacy one >>>".to_owned(),
            end_marker: "# <<< legacy one <<<".to_owned(),
            body: "source '/tmp/one'".to_owned(),
        };
        let second = ManagedBlock {
            start_marker: "# >>> legacy two >>>".to_owned(),
            end_marker: "# <<< legacy two <<<".to_owned(),
            body: "source '/tmp/two'".to_owned(),
        };
        fs::write(
            &profile,
            format!(
                "{}{}{}\n{}\n",
                first.render(),
                second.start_marker,
                "\nsource '/tmp/two'\n",
                "echo keep"
            ),
        )
        .expect("profile should be writable");

        let error = remove_all(&profile, &[first.clone(), second.clone()])
            .expect_err("remove_all should fail");

        assert!(matches!(error, crate::Error::ManagedBlockMissingEnd { .. }));

        let rendered = fs::read_to_string(profile).expect("profile should remain readable");
        assert!(rendered.contains(&first.start_marker));
        assert!(rendered.contains(&second.start_marker));
        assert!(rendered.contains("echo keep"));
    }

    #[test]
    fn migrate_blocks_is_atomic_when_managed_block_is_malformed() {
        let temp_root = crate::tests::temp_dir("managed-block-migrate-atomic");
        let profile = temp_root.join(".shellrc");
        let legacy = ManagedBlock {
            start_marker: "# >>> legacy >>>".to_owned(),
            end_marker: "# <<< legacy <<<".to_owned(),
            body: "source '/tmp/legacy'".to_owned(),
        };
        let managed = ManagedBlock {
            start_marker: "# >>> shellcomp bash tool >>>".to_owned(),
            end_marker: "# <<< shellcomp bash tool <<<".to_owned(),
            body: "source '/tmp/tool'".to_owned(),
        };
        fs::write(
            &profile,
            format!(
                "{}# >>> shellcomp bash tool >>>\nsource '/tmp/bad'\n",
                legacy.render()
            ),
        )
        .expect("profile should be writable");

        let error = migrate_blocks(&profile, std::slice::from_ref(&legacy), &managed)
            .expect_err("migration should fail");

        assert!(matches!(error, crate::Error::ManagedBlockMissingEnd { .. }));

        let rendered = fs::read_to_string(profile).expect("profile should remain readable");
        assert!(rendered.contains(&legacy.start_marker));
        assert!(rendered.contains("source '/tmp/bad'"));
        assert!(!rendered.contains("source '/tmp/tool'"));
    }

    #[test]
    fn migrate_blocks_reports_created_when_shellcomp_block_is_added_to_existing_profile() {
        let temp_root = crate::tests::temp_dir("managed-block-migrate-created");
        let profile = temp_root.join(".shellrc");
        let legacy = ManagedBlock {
            start_marker: "# >>> legacy >>>".to_owned(),
            end_marker: "# <<< legacy <<<".to_owned(),
            body: "source '/tmp/legacy'".to_owned(),
        };
        let managed = ManagedBlock {
            start_marker: "# >>> shellcomp bash tool >>>".to_owned(),
            end_marker: "# <<< shellcomp bash tool <<<".to_owned(),
            body: "source '/tmp/tool'".to_owned(),
        };
        fs::write(&profile, legacy.render()).expect("profile should be writable");

        let (legacy_change, managed_change) =
            migrate_blocks(&profile, std::slice::from_ref(&legacy), &managed)
                .expect("migration should succeed");

        assert_eq!(legacy_change, FileChange::Removed);
        assert_eq!(managed_change, FileChange::Created);
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    fn block(name: &str) -> ManagedBlock {
        ManagedBlock {
            start_marker: format!("# start {name}"),
            end_marker: format!("# end {name}"),
            body: format!("echo {name}"),
        }
    }

    #[test]
    fn preserves_marker_substrings_and_all_unmanaged_bytes() {
        let root = crate::tests::temp_dir("marker-substrings");
        let path = root.join("rc");
        let block = block("tool");
        let before = "echo '# start tool'\necho '# end tool'\n\n";
        let after = "\n\necho keep  \r\n\r\n";
        fs::write(&path, format!("{before}{}{after}", block.render())).unwrap();
        assert!(matches(&path, &block).unwrap());
        assert_eq!(remove(&path, &block).unwrap(), FileChange::Removed);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("{before}{after}")
        );
        assert_eq!(remove(&path, &block).unwrap(), FileChange::Absent);
    }

    #[test]
    fn crlf_profiles_match_and_remain_idempotent() {
        let root = crate::tests::temp_dir("marker-crlf");
        let path = root.join("rc");
        let block = block("tool");
        let original = format!(
            "echo before\r\n{}\r\necho after\r\n",
            block.render().replace('\n', "\r\n")
        );
        fs::write(&path, &original).unwrap();
        assert!(matches(&path, &block).unwrap());
        assert_eq!(upsert(&path, &block).unwrap(), FileChange::Unchanged);
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn malformed_markers_fail_without_changing_the_profile() {
        let root = crate::tests::temp_dir("marker-invalid");
        let path = root.join("rc");
        for (start, end) in [
            ("", ""),
            (" ", "end"),
            ("start\nnext", "end"),
            ("same", "same"),
        ] {
            fs::write(&path, "echo keep\n").unwrap();
            let legacy = ManagedBlock {
                start_marker: start.into(),
                end_marker: end.into(),
                body: String::new(),
            };
            assert!(migrate_blocks(&path, &[legacy], &block("tool")).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), "echo keep\n");
        }
    }

    #[test]
    fn nested_overlapping_and_orphan_markers_fail_without_writing() {
        let root = crate::tests::temp_dir("marker-overlap");
        let path = root.join("rc");
        let blocks = [block("one"), block("two")];
        for contents in [
            "# start one\n# start one\n# end one\n# end one\n",
            "# start one\n# start two\n# end one\n# end two\n",
            "# start one\n# start two\n# end two\n# end one\n",
            "# end one\n",
        ] {
            fs::write(&path, contents).unwrap();
            assert!(remove_all(&path, &blocks).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), contents);
        }
    }

    #[test]
    fn concurrent_programs_keep_every_profile_block() {
        let root = crate::tests::temp_dir("marker-concurrency");
        let path = root.join("rc");
        fs::write(&path, "echo keep\n").unwrap();
        let barrier = Arc::new(Barrier::new(24));
        std::thread::scope(|scope| {
            for index in 0..24 {
                let path = &path;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    upsert(path, &block(&format!("tool{index}"))).unwrap();
                });
            }
        });
        for index in 0..24 {
            assert!(matches(&path, &block(&format!("tool{index}"))).unwrap());
        }
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_profiles_without_modifying_their_destination() {
        let root = crate::tests::temp_dir("marker-symlink");
        let target = root.join("target");
        let profile = root.join("rc");
        fs::write(&target, "echo keep\n").unwrap();
        std::os::unix::fs::symlink(&target, &profile).unwrap();
        assert!(upsert(&profile, &block("tool")).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "echo keep\n");
        assert!(fs::symlink_metadata(&profile).unwrap().is_symlink());
    }
}
