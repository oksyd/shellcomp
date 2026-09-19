use crate::error::{Error, Result};
use crate::infra::{env::Environment, paths};
use crate::model::{
    FailureKind, MigrateManagedBlocksReport, MigrateManagedBlocksRequest, Operation,
};
use crate::service::{
    FailureContext, failure, home_env_hint, push_unique, resolve_target_path, with_operation_lock,
    with_operation_observation, zsh_target_is_autoloadable,
};
use crate::shell;

pub(crate) fn execute(
    env: &Environment,
    request: MigrateManagedBlocksRequest<'_>,
) -> Result<MigrateManagedBlocksReport> {
    let shell = request.shell.clone();
    let program_name = request.program_name;
    with_operation_observation(
        Operation::MigrateManagedBlocks,
        &shell,
        program_name,
        None,
        || {
            paths::validate_program_name(program_name)?;
            let target_path = resolve_target_path(
                env,
                &request.shell,
                request.program_name,
                request.path_override.as_deref(),
            )
            .map_err(|error| map_resolve_error(env, &request, error))?;
            let lock_path = target_path.clone();
            with_operation_lock(&lock_path, || {
                validate_migration_target(&request, &target_path)?;

                let mut affected_locations = Vec::new();
                push_unique(&mut affected_locations, target_path.clone());

                let outcome = shell::migrate_managed_blocks(
                    env,
                    &request.shell,
                    program_name,
                    &target_path,
                    &request.legacy_blocks,
                )
                .map_err(|error| map_migration_error(env, &request, &target_path, error))?;

                for path in outcome.affected_locations {
                    push_unique(&mut affected_locations, path);
                }

                Ok(MigrateManagedBlocksReport {
                    shell: request.shell,
                    target_path,
                    location: outcome.location,
                    legacy_change: outcome.legacy_change,
                    managed_change: outcome.managed_change,
                    affected_locations,
                })
            })
        },
        |report| Some(report.target_path.clone()),
    )
}

fn map_resolve_error(
    env: &Environment,
    request: &MigrateManagedBlocksRequest<'_>,
    error: Error,
) -> Error {
    error.with_context(|error| {
        let uses_default_target = request.path_override.is_none();

        Some(match error {
            Error::MissingHome => failure(
                FailureContext {
                    operation: Operation::MigrateManagedBlocks,
                    shell: &request.shell,
                    target_path: request.path_override.as_deref(),
                    affected_locations: Vec::new(),
                    kind: FailureKind::MissingHome,
                },
                format!(
                    "Could not resolve the managed completion path for block migration because {} is not set.",
                    home_env_hint(env, &request.shell)
                ),
                Some(
                    format!(
                        "Set {} for the current process or pass `path_override` so shellcomp can resolve the target completion path.",
                        home_env_hint(env, &request.shell)
                    ),
                ),
            ),
            Error::PathHasNoParent { path } => failure(
                FailureContext {
                    operation: Operation::MigrateManagedBlocks,
                    shell: &request.shell,
                    target_path: Some(path),
                    affected_locations: vec![path.clone()],
                    kind: FailureKind::InvalidTargetPath,
                },
                format!(
                    "The requested migration path `{}` does not have a parent directory.",
                    path.display()
                ),
                Some(
                    "Pass a file path with a real parent directory so shellcomp can build the replacement managed block."
                        .to_owned(),
                ),
            ),
            Error::InvalidTargetPath { path, reason } => failure(
                FailureContext {
                    operation: Operation::MigrateManagedBlocks,
                    shell: &request.shell,
                    target_path: Some(path),
                    affected_locations: vec![path.clone()],
                    kind: if uses_default_target {
                        FailureKind::DefaultPathUnavailable
                    } else {
                        FailureKind::InvalidTargetPath
                    },
                },
                if uses_default_target {
                    format!(
                        "The managed default completion path `{}` is invalid: {reason}.",
                        path.display()
                    )
                } else {
                    format!("The requested migration path `{}` is invalid: {reason}.", path.display())
                },
                Some(
                    "Choose an absolute, non-symlink, normalized migration target path with an existing parent directory."
                        .to_owned(),
                ),
            ),
            Error::UnsupportedShell(shell) => failure(
                FailureContext {
                    operation: Operation::MigrateManagedBlocks,
                    shell,
                    target_path: request.path_override.as_deref(),
                    affected_locations: Vec::new(),
                    kind: FailureKind::UnsupportedShell,
                },
                format!("Shell `{shell}` is not implemented in the current production support set."),
                None,
            ),
            _ => return None,
        })
    })
}

fn map_migration_error(
    env: &Environment,
    request: &MigrateManagedBlocksRequest<'_>,
    target_path: &std::path::Path,
    error: Error,
) -> Error {
    error.with_context(|error| {
        let startup_path = error.location().map(std::path::Path::to_path_buf);
        Some(match error {
            Error::MissingHome => failure(
                FailureContext {
                    operation: Operation::MigrateManagedBlocks,
                    shell: &request.shell,
                    target_path: Some(target_path),
                    affected_locations: vec![target_path.to_path_buf()],
                    kind: FailureKind::MissingHome,
                },
                format!(
                    "Could not resolve the managed {} startup file during block migration because {} is not set.",
                    request.shell,
                    home_env_hint(env, &request.shell)
                ),
                Some(format!(
                    "Set {} for the current process or rewrite the startup block manually.",
                    home_env_hint(env, &request.shell)
                )),
            ),
            Error::Io { path, .. } | Error::InvalidUtf8File { path } | Error::InvalidTargetPath { path, .. } => failure(
                FailureContext {
                    operation: Operation::MigrateManagedBlocks,
                    shell: &request.shell,
                    target_path: Some(target_path),
                    affected_locations: vec![target_path.to_path_buf(), path.clone()],
                    kind: FailureKind::ProfileUnavailable,
                },
                format!(
                    "Could not rewrite the managed {} startup file during block migration.",
                    request.shell
                ),
                Some(match startup_path.as_deref() {
                    Some(path) => format!(
                        "Review {} manually and remove or replace the legacy block yourself.",
                        path.display()
                    ),
                    None => {
                        "Review the relevant shell startup file manually and remove or replace the legacy block yourself."
                            .to_owned()
                    }
                }),
            ),
            Error::ManagedBlockMissingEnd { path, .. } | Error::InvalidManagedBlock { path, .. } => failure(
                FailureContext {
                    operation: Operation::MigrateManagedBlocks,
                    shell: &request.shell,
                    target_path: Some(target_path),
                    affected_locations: vec![target_path.to_path_buf(), path.clone()],
                    kind: FailureKind::ProfileCorrupted,
                },
                format!(
                    "Could not rewrite the managed {} startup file because a managed block is malformed.",
                    request.shell
                ),
                Some(match startup_path.as_deref() {
                    Some(path) => format!(
                        "Repair or remove the malformed block in {} manually, then re-run migration.",
                        path.display()
                    ),
                    None => "Repair or remove the malformed block manually, then re-run migration."
                        .to_owned(),
                }),
            ),
            Error::NonUtf8Path { path } => failure(
                FailureContext {
                    operation: Operation::MigrateManagedBlocks,
                    shell: &request.shell,
                    target_path: Some(target_path),
                    affected_locations: vec![target_path.to_path_buf(), path.clone()],
                    kind: FailureKind::InvalidTargetPath,
                },
                "The target completion path could not be represented safely for managed block migration.",
                Some(
                    "Move the completion file to a UTF-8 path before migrating managed shell blocks."
                        .to_owned(),
                ),
            ),
            _ => return None,
        })
    })
}

fn validate_migration_target(
    request: &MigrateManagedBlocksRequest<'_>,
    target_path: &std::path::Path,
) -> Result<()> {
    if matches!(request.shell, crate::Shell::Zsh)
        && !zsh_target_is_autoloadable(request.program_name, target_path)
    {
        let expected = format!("_{}", request.program_name);
        return Err(failure(
            FailureContext {
                operation: Operation::MigrateManagedBlocks,
                shell: &request.shell,
                target_path: Some(target_path),
                affected_locations: vec![target_path.to_path_buf()],
                kind: FailureKind::InvalidTargetPath,
            },
            format!(
                "The requested zsh migration target `{}` is not autoloadable because its file name is not `{expected}`.",
                target_path.display()
            ),
            Some(format!(
                "Rename the completion file to `{expected}` or choose an autoloadable target path before migrating managed zsh blocks."
            )),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::execute;
    use crate::infra::env::Environment;
    use crate::model::{
        FileChange, LegacyManagedBlock, MigrateManagedBlocksRequest, Operation, Shell,
    };

    #[test]
    fn migrate_rewrites_legacy_bash_block() {
        let temp_root = crate::tests::temp_dir("migrate-bash-legacy");
        let home = temp_root.join("home");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();
        fs::create_dir_all(&home).expect("home should be creatable");
        fs::write(
            home.join(".bashrc"),
            "# >>> legacy bash >>>\n. '/tmp/tool'\n# <<< legacy bash <<<\n",
        )
        .expect(".bashrc should be writable");

        let report = execute(
            &env,
            MigrateManagedBlocksRequest {
                shell: Shell::Bash,
                program_name: "tool",
                path_override: None,
                legacy_blocks: vec![LegacyManagedBlock {
                    start_marker: "# >>> legacy bash >>>".to_owned(),
                    end_marker: "# <<< legacy bash <<<".to_owned(),
                }],
            },
        )
        .expect("migration should succeed");

        assert_eq!(report.legacy_change, FileChange::Removed);
        assert_eq!(report.managed_change, FileChange::Created);
        assert!(matches!(
            report.location.as_deref(),
            Some(path) if path.ends_with(".bashrc")
        ));
        let bashrc =
            fs::read_to_string(home.join(".bashrc")).expect(".bashrc should remain readable");
        assert!(bashrc.contains("shellcomp bash tool"));
        assert!(!bashrc.contains("legacy bash"));
    }

    #[test]
    fn migrate_returns_noop_for_fish() {
        let temp_root = crate::tests::temp_dir("migrate-fish-noop");
        let home = temp_root.join("home");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_CONFIG_HOME")
            .without_real_path_lookups();

        let report = execute(
            &env,
            MigrateManagedBlocksRequest {
                shell: Shell::Fish,
                program_name: "tool",
                path_override: None,
                legacy_blocks: vec![LegacyManagedBlock {
                    start_marker: "# >>> legacy fish >>>".to_owned(),
                    end_marker: "# <<< legacy fish <<<".to_owned(),
                }],
            },
        )
        .expect("migration should succeed");

        assert_eq!(report.legacy_change, FileChange::Absent);
        assert_eq!(report.managed_change, FileChange::Absent);
        assert!(report.location.is_none());
    }

    #[test]
    fn migrate_returns_structured_failure_for_invalid_path() {
        let error = execute(
            &Environment::test().without_real_path_lookups(),
            MigrateManagedBlocksRequest {
                shell: Shell::Bash,
                program_name: "tool",
                path_override: Some(PathBuf::from("/")),
                legacy_blocks: Vec::new(),
            },
        )
        .expect_err("migration should fail");

        let report = crate::tests::assert_structural_failure(error, "migrate");
        assert_eq!(report.operation, Operation::MigrateManagedBlocks);
        assert_eq!(report.kind, crate::FailureKind::InvalidTargetPath);
    }

    #[test]
    fn migrate_rejects_relative_target_path_override() {
        let target = std::path::PathBuf::from("custom.tool");
        let error = execute(
            &Environment::test().without_real_path_lookups(),
            MigrateManagedBlocksRequest {
                shell: Shell::Bash,
                program_name: "tool",
                path_override: Some(target.clone()),
                legacy_blocks: Vec::new(),
            },
        )
        .expect_err("migrate should reject relative target");

        let report = crate::tests::assert_structural_failure(error, "migrate");
        assert_eq!(report.operation, Operation::MigrateManagedBlocks);
        assert_eq!(report.kind, crate::FailureKind::InvalidTargetPath);
        assert_eq!(report.target_path, Some(target));
    }

    #[test]
    fn migrate_rejects_relative_default_target_path_from_environment() {
        let env = Environment::test()
            .with_var("HOME", "relative-home")
            .with_var("XDG_DATA_HOME", "relative-cache")
            .without_real_path_lookups();

        let error = execute(
            &env,
            MigrateManagedBlocksRequest {
                shell: Shell::Bash,
                program_name: "tool",
                path_override: None,
                legacy_blocks: Vec::new(),
            },
        )
        .expect_err("migrate should fail with invalid default path");

        let report = crate::tests::assert_structural_failure(error, "migrate");
        assert_eq!(report.kind, crate::FailureKind::DefaultPathUnavailable);
        assert!(report.reason.contains("managed default completion path"));
    }

    #[cfg(unix)]
    #[test]
    fn migrate_rejects_symlink_path_segments_in_target_override() {
        use std::os::unix::fs::symlink;

        let temp_root = crate::tests::temp_dir("migrate-symlink-path");
        let real_dir = temp_root.join("real");
        let link_dir = temp_root.join("link");
        let target = link_dir.join("tool.bash");

        std::fs::create_dir_all(&real_dir).expect("real dir should be creatable");
        symlink(&real_dir, &link_dir).expect("symlink should be created");

        let error = execute(
            &Environment::test().without_real_path_lookups(),
            MigrateManagedBlocksRequest {
                shell: Shell::Bash,
                program_name: "tool",
                path_override: Some(target.clone()),
                legacy_blocks: Vec::new(),
            },
        )
        .expect_err("migrate should reject symlink path");

        let report = crate::tests::assert_structural_failure(error, "migrate");
        assert_eq!(report.operation, Operation::MigrateManagedBlocks);
        assert_eq!(report.kind, crate::FailureKind::InvalidTargetPath);
        assert_eq!(report.target_path, Some(target));
    }

    #[test]
    fn migrate_rejects_non_autoloadable_zsh_target_without_rewriting_legacy_block() {
        let temp_root = crate::tests::temp_dir("migrate-zsh-non-autoloadable");
        let home = temp_root.join("home");
        let zshrc = home.join(".zshrc");
        fs::create_dir_all(&home).expect("home should be creatable");
        fs::write(
            &zshrc,
            "# >>> legacy zsh >>>\nfpath=(/tmp/tool $fpath)\n# <<< legacy zsh <<<\n",
        )
        .expect(".zshrc should be writable");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("ZDOTDIR")
            .without_real_path_lookups();
        let target = temp_root.join("custom").join("tool.zsh");

        let error = execute(
            &env,
            MigrateManagedBlocksRequest {
                shell: Shell::Zsh,
                program_name: "tool",
                path_override: Some(target.clone()),
                legacy_blocks: vec![LegacyManagedBlock {
                    start_marker: "# >>> legacy zsh >>>".to_owned(),
                    end_marker: "# <<< legacy zsh <<<".to_owned(),
                }],
            },
        )
        .expect_err("migration should fail");

        let report = crate::tests::assert_structural_failure(error, "migrate");
        assert_eq!(report.operation, Operation::MigrateManagedBlocks);
        assert_eq!(report.kind, crate::FailureKind::InvalidTargetPath);
        assert_eq!(report.target_path, Some(target));

        let rendered = fs::read_to_string(zshrc).expect(".zshrc should remain readable");
        assert!(rendered.contains("legacy zsh"));
        assert!(!rendered.contains("shellcomp zsh tool"));
    }

    #[cfg(unix)]
    #[test]
    fn migrate_keeps_legacy_bash_block_when_target_path_is_non_utf8() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let temp_root = crate::tests::temp_dir("migrate-bash-non-utf8-target");
        let home = temp_root.join("home");
        let bashrc = home.join(".bashrc");
        fs::create_dir_all(&home).expect("home should be creatable");
        fs::write(
            &bashrc,
            "# >>> legacy bash >>>\n. '/tmp/tool'\n# <<< legacy bash <<<\n",
        )
        .expect(".bashrc should be writable");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();
        let target = temp_root.join(OsString::from_vec(b"tool-\xff".to_vec()));

        let error = execute(
            &env,
            MigrateManagedBlocksRequest {
                shell: Shell::Bash,
                program_name: "tool",
                path_override: Some(target.clone()),
                legacy_blocks: vec![LegacyManagedBlock {
                    start_marker: "# >>> legacy bash >>>".to_owned(),
                    end_marker: "# <<< legacy bash <<<".to_owned(),
                }],
            },
        )
        .expect_err("migration should fail");

        let report = crate::tests::assert_structural_failure(error, "migrate");
        assert_eq!(report.operation, Operation::MigrateManagedBlocks);
        assert_eq!(report.kind, crate::FailureKind::InvalidTargetPath);
        assert_eq!(report.target_path, Some(target));

        let rendered = fs::read_to_string(bashrc).expect(".bashrc should remain readable");
        assert!(rendered.contains("legacy bash"));
        assert!(!rendered.contains("shellcomp bash tool"));
    }

    #[cfg(unix)]
    #[test]
    fn migrate_keeps_legacy_zsh_block_when_parent_directory_is_non_utf8() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let temp_root = crate::tests::temp_dir("migrate-zsh-non-utf8-parent");
        let home = temp_root.join("home");
        let zshrc = home.join(".zshrc");
        fs::create_dir_all(&home).expect("home should be creatable");
        fs::write(
            &zshrc,
            "# >>> legacy zsh >>>\nfpath=(/tmp/tool $fpath)\n# <<< legacy zsh <<<\n",
        )
        .expect(".zshrc should be writable");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("ZDOTDIR")
            .without_real_path_lookups();
        let target = temp_root
            .join(OsString::from_vec(b"zfunc-\xff".to_vec()))
            .join("_tool");

        let error = execute(
            &env,
            MigrateManagedBlocksRequest {
                shell: Shell::Zsh,
                program_name: "tool",
                path_override: Some(target.clone()),
                legacy_blocks: vec![LegacyManagedBlock {
                    start_marker: "# >>> legacy zsh >>>".to_owned(),
                    end_marker: "# <<< legacy zsh <<<".to_owned(),
                }],
            },
        )
        .expect_err("migration should fail");

        let report = crate::tests::assert_structural_failure(error, "migrate");
        assert_eq!(report.operation, Operation::MigrateManagedBlocks);
        assert_eq!(report.kind, crate::FailureKind::InvalidTargetPath);
        assert_eq!(report.target_path, Some(target));

        let rendered = fs::read_to_string(zshrc).expect(".zshrc should remain readable");
        assert!(rendered.contains("legacy zsh"));
        assert!(!rendered.contains("shellcomp zsh tool"));
    }

    #[test]
    fn migrate_does_not_partially_remove_legacy_blocks_when_a_later_one_is_malformed() {
        let temp_root = crate::tests::temp_dir("migrate-bash-legacy-atomic");
        let home = temp_root.join("home");
        let bashrc = home.join(".bashrc");
        fs::create_dir_all(&home).expect("home should be creatable");
        fs::write(
            &bashrc,
            "# >>> legacy one >>>\n. '/tmp/one'\n# <<< legacy one <<<\n# >>> legacy two >>>\n. '/tmp/two'\n",
        )
        .expect(".bashrc should be writable");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();

        let error = execute(
            &env,
            MigrateManagedBlocksRequest {
                shell: Shell::Bash,
                program_name: "tool",
                path_override: None,
                legacy_blocks: vec![
                    LegacyManagedBlock {
                        start_marker: "# >>> legacy one >>>".to_owned(),
                        end_marker: "# <<< legacy one <<<".to_owned(),
                    },
                    LegacyManagedBlock {
                        start_marker: "# >>> legacy two >>>".to_owned(),
                        end_marker: "# <<< legacy two <<<".to_owned(),
                    },
                ],
            },
        )
        .expect_err("migration should fail");

        let report = crate::tests::assert_structural_failure(error, "migrate");
        assert_eq!(report.operation, Operation::MigrateManagedBlocks);
        assert_eq!(report.kind, crate::FailureKind::ProfileCorrupted);

        let rendered = fs::read_to_string(bashrc).expect(".bashrc should remain readable");
        assert!(rendered.contains("legacy one"));
        assert!(rendered.contains("legacy two"));
        assert!(!rendered.contains("shellcomp bash tool"));
    }

    #[test]
    fn migrate_does_not_remove_legacy_block_when_existing_shellcomp_block_is_malformed() {
        let temp_root = crate::tests::temp_dir("migrate-bash-existing-shellcomp-corrupt");
        let home = temp_root.join("home");
        let bashrc = home.join(".bashrc");
        fs::create_dir_all(&home).expect("home should be creatable");
        fs::write(
            &bashrc,
            "# >>> legacy bash >>>\n. '/tmp/tool'\n# <<< legacy bash <<<\n# >>> shellcomp bash tool >>>\nif [ -f '/tmp/bad' ]; then\n  . '/tmp/bad'\nfi\n",
        )
        .expect(".bashrc should be writable");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();

        let error = execute(
            &env,
            MigrateManagedBlocksRequest {
                shell: Shell::Bash,
                program_name: "tool",
                path_override: None,
                legacy_blocks: vec![LegacyManagedBlock {
                    start_marker: "# >>> legacy bash >>>".to_owned(),
                    end_marker: "# <<< legacy bash <<<".to_owned(),
                }],
            },
        )
        .expect_err("migration should fail");

        let report = crate::tests::assert_structural_failure(error, "migrate");
        assert_eq!(report.operation, Operation::MigrateManagedBlocks);
        assert_eq!(report.kind, crate::FailureKind::ProfileCorrupted);
        assert!(
            report
                .next_step
                .as_deref()
                .is_some_and(|text| text.contains(".bashrc"))
        );

        let rendered = fs::read_to_string(bashrc).expect(".bashrc should remain readable");
        assert!(rendered.contains("legacy bash"));
        assert!(rendered.contains("shellcomp bash tool"));
        assert!(!rendered.contains(".local/share/bash-completion/completions/tool"));
    }
}
