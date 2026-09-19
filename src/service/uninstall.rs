use crate::error::{Error, Result};
use crate::infra::{env::Environment, fs, paths};
use crate::model::{
    ActivationPolicy, CleanupReport, FailureKind, FileChange, Operation, RemoveReport,
    UninstallRequest,
};
use crate::service::{
    FailureContext, FailureStatus, failure, failure_with_status, home_env_hint,
    legacy_activation_policy, push_unique, resolve_target_path, should_use_shell_backend,
    with_operation_lock, with_operation_observation,
};
use crate::shell;

pub(crate) fn execute(env: &Environment, request: UninstallRequest<'_>) -> Result<RemoveReport> {
    let activation_policy = legacy_activation_policy(
        env,
        &request.shell,
        request.program_name,
        request.path_override.as_deref(),
    );

    execute_with_policy(env, request, activation_policy)
}

pub(crate) fn execute_with_policy(
    env: &Environment,
    request: UninstallRequest<'_>,
    activation_policy: ActivationPolicy,
) -> Result<RemoveReport> {
    let shell = request.shell.clone();
    let program_name = request.program_name;
    with_operation_observation(
        Operation::Uninstall,
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
                let file_change = fs::remove_file_if_exists(&target_path)
                    .map_err(|error| map_file_error(&request, &target_path, error))?;

                let mut affected_locations = Vec::new();
                push_unique(&mut affected_locations, target_path.clone());

                let cleanup = if should_use_shell_backend(
                    env,
                    &request.shell,
                    program_name,
                    activation_policy,
                    &target_path,
                ) {
                    let outcome = shell::uninstall_default(
                        env,
                        &request.shell,
                        request.program_name,
                        &target_path,
                    )
                    .map_err(|error| {
                        map_cleanup_error(env, &request, &target_path, file_change, error)
                    })?;
                    for path in outcome.affected_locations {
                        push_unique(&mut affected_locations, path);
                    }
                    outcome.cleanup
                } else {
                    CleanupReport {
                            mode: crate::ActivationMode::Manual,
                            change: crate::FileChange::Absent,
                            location: None,
                            reason: Some(
                                "Managed activation cleanup was skipped because the activation policy is manual."
                                    .to_owned(),
                            ),
                            next_step: None,
                        }
                };

                Ok(RemoveReport {
                    shell: request.shell,
                    target_path,
                    file_change,
                    cleanup,
                    affected_locations,
                })
            })
        },
        |report| Some(report.target_path.clone()),
    )
}

fn map_resolve_error(env: &Environment, request: &UninstallRequest<'_>, error: Error) -> Error {
    error.with_context(|error| {
        let uses_default_target = request.path_override.is_none();

        Some(match error {
            Error::MissingHome => failure(
                FailureContext {
                    operation: Operation::Uninstall,
                    shell: &request.shell,
                    target_path: request.path_override.as_deref(),
                    affected_locations: Vec::new(),
                    kind: FailureKind::MissingHome,
                },
                format!(
                    "Could not resolve the managed completion path because {} is not set.",
                    home_env_hint(env, &request.shell)
                ),
                Some(
                    format!(
                        "Set {} for the current process or pass the exact `path_override` that should be removed.",
                        home_env_hint(env, &request.shell)
                    ),
                ),
            ),
            Error::PathHasNoParent { path } => failure(
                FailureContext {
                    operation: Operation::Uninstall,
                    shell: &request.shell,
                    target_path: Some(path),
                    affected_locations: vec![path.clone()],
                    kind: FailureKind::InvalidTargetPath,
                },
                format!(
                    "The requested uninstall path `{}` does not have a parent directory.",
                    path.display()
                ),
                Some(
                    "Pass the exact file path that should be removed, including a real parent directory."
                        .to_owned(),
                ),
            ),
            Error::InvalidTargetPath { path, reason } => failure(
                FailureContext {
                    operation: Operation::Uninstall,
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
                    format!("The requested uninstall path `{}` is invalid: {reason}.", path.display())
                },
                Some(
                    "Choose an absolute, non-symlink, normalized custom target path with an existing parent directory."
                        .to_owned(),
                ),
            ),
            Error::UnsupportedShell(shell) => failure(
                FailureContext {
                    operation: Operation::Uninstall,
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

fn map_file_error(
    request: &UninstallRequest<'_>,
    target_path: &std::path::Path,
    error: Error,
) -> Error {
    error.with_context(|error| {
        Some(match error {
        Error::Io { action, .. } => failure(
            FailureContext {
                operation: Operation::Uninstall,
                shell: &request.shell,
                target_path: Some(target_path),
                affected_locations: vec![target_path.to_path_buf()],
                kind: match *action {
                    "remove file" => FailureKind::CompletionTargetUnavailable,
                    _ => FailureKind::CompletionFileUnreadable,
                },
            },
            format!(
                "Could not remove the {} completion file at `{}`.",
                request.shell,
                target_path.display()
            ),
            Some(
                "Remove the file manually or fix the file permissions, then run uninstall again."
                    .to_owned(),
            ),
        ),
        _ => return None,
    })
    })
}

fn map_cleanup_error(
    env: &Environment,
    request: &UninstallRequest<'_>,
    target_path: &std::path::Path,
    file_change: FileChange,
    error: Error,
) -> Error {
    error.with_context(|error| {
        let cleanup_path = error.location().map(std::path::Path::to_path_buf);
        Some(match error {
            Error::MissingHome => failure_with_status(
                FailureContext {
                    operation: Operation::Uninstall,
                    shell: &request.shell,
                    target_path: Some(target_path),
                    affected_locations: vec![target_path.to_path_buf()],
                    kind: FailureKind::MissingHome,
                },
                FailureStatus {
                    file_change: Some(file_change),
                    activation: None,
                    cleanup: None,
                },
                format!(
                    "Could not resolve the managed {} startup file because {} is not set.",
                    request.shell,
                    home_env_hint(env, &request.shell)
                ),
                Some(format!(
                    "Set {} for the current process or remove the managed shell block manually.",
                    home_env_hint(env, &request.shell)
                )),
            ),
            Error::Io { path, .. } | Error::InvalidUtf8File { path } | Error::InvalidTargetPath { path, .. } => failure_with_status(
                FailureContext {
                    operation: Operation::Uninstall,
                    shell: &request.shell,
                    target_path: Some(target_path),
                    affected_locations: vec![target_path.to_path_buf(), path.clone()],
                    kind: FailureKind::ProfileUnavailable,
                },
                FailureStatus {
                    file_change: Some(file_change),
                    activation: None,
                    cleanup: None,
                },
                format!(
                    "Could not clean up the managed {} activation block.",
                    request.shell
                ),
                Some(match cleanup_path.as_deref() {
                    Some(path) => format!(
                        "Review {} manually and remove the shellcomp-managed block yourself.",
                        path.display()
                    ),
                    None => {
                        "Review the managed shell startup file manually and remove the shellcomp-managed block yourself."
                            .to_owned()
                    }
                }),
            ),
            Error::ManagedBlockMissingEnd { path, .. } | Error::InvalidManagedBlock { path, .. } => failure_with_status(
                FailureContext {
                    operation: Operation::Uninstall,
                    shell: &request.shell,
                    target_path: Some(target_path),
                    affected_locations: vec![target_path.to_path_buf(), path.clone()],
                    kind: FailureKind::ProfileCorrupted,
                },
                FailureStatus {
                    file_change: Some(file_change),
                    activation: None,
                    cleanup: None,
                },
                format!(
                    "Could not clean up the managed {} activation block.",
                    request.shell
                ),
                Some(match cleanup_path.as_deref() {
                    Some(path) => format!(
                        "Review {} manually and remove the shellcomp-managed block yourself.",
                        path.display()
                    ),
                    None => {
                        "Review the managed shell startup file manually and remove the shellcomp-managed block yourself."
                            .to_owned()
                    }
                }),
            ),
            _ => return None,
        })
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{execute, execute_with_policy};
    use crate::infra::env::Environment;
    use crate::model::{
        ActivationPolicy, FileChange, InstallRequest, Operation, Shell, UninstallRequest,
    };
    use crate::service::install;

    #[test]
    fn uninstall_removes_managed_bash_block() {
        let temp_root = crate::tests::temp_dir("uninstall-bash-managed");
        let home = temp_root.join("home");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();

        install::execute(
            &env,
            InstallRequest {
                shell: Shell::Bash,
                program_name: "tool",
                script: b"complete -F _tool tool\n",
                path_override: None,
            },
        )
        .expect("install should succeed");

        let report = execute(
            &env,
            UninstallRequest {
                shell: Shell::Bash,
                program_name: "tool",
                path_override: None,
            },
        )
        .expect("uninstall should succeed");

        assert_eq!(report.file_change, FileChange::Removed);
        assert_eq!(report.cleanup.change, FileChange::Removed);

        let bashrc = fs::read_to_string(home.join(".bashrc")).expect(".bashrc should exist");
        assert!(!bashrc.contains("shellcomp bash tool"));
    }

    #[test]
    fn uninstall_reports_system_loader_cleanup_for_loader_wired_bash() {
        let temp_root = crate::tests::temp_dir("uninstall-bash-system-loader");
        let home = temp_root.join("home");
        let completion_dir = home.join(".local/share/bash-completion/completions");
        let completion_path = completion_dir.join("tool");
        fs::create_dir_all(&completion_dir).expect("completion dir should be creatable");
        fs::write(&completion_path, "complete -F _tool tool\n")
            .expect("completion file should be writable");
        fs::write(
            home.join(".bashrc"),
            "source /usr/share/bash-completion/bash_completion\n",
        )
        .expect(".bashrc should be writable");

        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("BASH_COMPLETION_VERSINFO")
            .with_existing_path("/usr/share/bash-completion/bash_completion")
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();

        let report = execute(
            &env,
            UninstallRequest {
                shell: Shell::Bash,
                program_name: "tool",
                path_override: None,
            },
        )
        .expect("uninstall should succeed");

        assert_eq!(report.file_change, FileChange::Removed);
        assert_eq!(report.cleanup.mode, crate::ActivationMode::SystemLoader);
        assert_eq!(report.cleanup.change, FileChange::Absent);
        assert_eq!(report.cleanup.location, Some(home.join(".bashrc")));
    }

    #[test]
    fn uninstall_is_idempotent() {
        let temp_root = crate::tests::temp_dir("uninstall-idempotent");
        let home = temp_root.join("home");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();

        let report = execute(
            &env,
            UninstallRequest {
                shell: Shell::Fish,
                program_name: "tool",
                path_override: None,
            },
        )
        .expect("uninstall should succeed");

        assert_eq!(report.file_change, FileChange::Absent);
        assert_eq!(report.cleanup.mode, crate::ActivationMode::NativeDirectory);
        assert_eq!(report.cleanup.change, FileChange::Absent);
    }

    #[test]
    fn uninstall_with_path_override_does_not_touch_rc_files() {
        let temp_root = crate::tests::temp_dir("uninstall-custom-path");
        let target = temp_root.join("custom").join("tool.bash");
        fs::create_dir_all(target.parent().expect("custom path should have parent"))
            .expect("custom dir should be creatable");
        fs::write(&target, "complete -F _tool tool\n").expect("target file should exist");

        let report = execute(
            &Environment::test().without_real_path_lookups(),
            UninstallRequest {
                shell: Shell::Bash,
                program_name: "tool",
                path_override: Some(target.clone()),
            },
        )
        .expect("uninstall should succeed");

        assert_eq!(report.file_change, FileChange::Removed);
        assert_eq!(report.cleanup.mode, crate::ActivationMode::Manual);
        assert_eq!(report.cleanup.change, FileChange::Absent);
        assert!(!target.exists());
    }

    #[test]
    fn uninstall_returns_profile_corrupted_for_malformed_bash_block() {
        let temp_root = crate::tests::temp_dir("uninstall-bash-corrupted");
        let home = temp_root.join("home");
        let completion_dir = home.join(".local/share/bash-completion/completions");
        fs::create_dir_all(&completion_dir).expect("completion dir should be creatable");
        fs::write(
            home.join(".bashrc"),
            "# >>> shellcomp bash tool >>>\n. '/tmp/tool'\n",
        )
        .expect(".bashrc should be writable");

        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();

        let error = execute(
            &env,
            UninstallRequest {
                shell: Shell::Bash,
                program_name: "tool",
                path_override: None,
            },
        )
        .expect_err("uninstall should fail");

        let report = crate::tests::assert_structural_failure(error, "uninstall");
        assert_eq!(report.operation, Operation::Uninstall);
        assert_eq!(report.kind, crate::FailureKind::ProfileCorrupted);
        assert_eq!(report.target_path, Some(completion_dir.join("tool")));
        assert!(report.cleanup.is_none());
        assert!(
            report
                .affected_locations
                .iter()
                .any(|path| path.ends_with(".bashrc"))
        );
        assert!(report.next_step.is_some());
        assert!(
            report
                .next_step
                .as_deref()
                .is_some_and(|text| text.contains(".bashrc"))
        );
    }

    #[test]
    fn uninstall_preserves_file_change_when_cleanup_fails() {
        let temp_root = crate::tests::temp_dir("uninstall-bash-partial-failure");
        let home = temp_root.join("home");
        let completion_dir = home.join(".local/share/bash-completion/completions");
        let completion_path = completion_dir.join("tool");
        fs::create_dir_all(&completion_dir).expect("completion dir should be creatable");
        fs::write(&completion_path, "complete -F _tool tool\n")
            .expect("completion file should be writable");
        fs::write(
            home.join(".bashrc"),
            "# >>> shellcomp bash tool >>>\n. '/tmp/tool'\n",
        )
        .expect(".bashrc should be writable");

        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();

        let error = execute(
            &env,
            UninstallRequest {
                shell: Shell::Bash,
                program_name: "tool",
                path_override: None,
            },
        )
        .expect_err("uninstall should fail");

        let report = crate::tests::assert_structural_failure(error, "uninstall");
        assert_eq!(report.operation, Operation::Uninstall);
        assert_eq!(report.kind, crate::FailureKind::ProfileCorrupted);
        assert_eq!(report.target_path, Some(completion_path.clone()));
        assert_eq!(report.file_change, Some(FileChange::Removed));
        assert!(!completion_path.exists());
    }

    #[test]
    fn uninstall_with_custom_path_can_clean_managed_bash_activation() {
        let temp_root = crate::tests::temp_dir("uninstall-custom-bash-managed");
        let home = temp_root.join("home");
        let target = temp_root.join("custom").join("tool.bash");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();

        install::execute_with_policy(
            &env,
            InstallRequest {
                shell: Shell::Bash,
                program_name: "tool",
                script: b"complete -F _tool tool\n",
                path_override: Some(target.clone()),
            },
            ActivationPolicy::AutoManaged,
        )
        .expect("install should succeed");

        let report = execute_with_policy(
            &env,
            UninstallRequest {
                shell: Shell::Bash,
                program_name: "tool",
                path_override: Some(target),
            },
            ActivationPolicy::AutoManaged,
        )
        .expect("uninstall should succeed");

        assert_eq!(report.cleanup.mode, crate::ActivationMode::ManagedRcBlock);
        assert_eq!(report.cleanup.change, FileChange::Removed);
        let bashrc = fs::read_to_string(home.join(".bashrc")).expect(".bashrc should exist");
        assert!(!bashrc.contains("shellcomp bash tool"));
    }

    #[test]
    fn uninstall_with_manual_policy_keeps_default_fish_cleanup_native() {
        let temp_root = crate::tests::temp_dir("uninstall-default-fish-manual-policy");
        let home = temp_root.join("home");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_CONFIG_HOME")
            .without_real_path_lookups();

        let report = execute_with_policy(
            &env,
            UninstallRequest {
                shell: Shell::Fish,
                program_name: "tool",
                path_override: None,
            },
            ActivationPolicy::Manual,
        )
        .expect("uninstall should succeed");

        assert_eq!(report.cleanup.mode, crate::ActivationMode::NativeDirectory);
        assert_eq!(report.cleanup.change, FileChange::Absent);
    }

    #[test]
    fn uninstall_with_explicit_default_fish_path_keeps_native_cleanup_mode() {
        let temp_root = crate::tests::temp_dir("uninstall-explicit-default-fish-path");
        let home = temp_root.join("home");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_CONFIG_HOME")
            .without_real_path_lookups();
        let target = home.join(".config/fish/completions/tool.fish");
        fs::create_dir_all(target.parent().expect("default path should have a parent"))
            .expect("default completion dir should be creatable");
        fs::write(&target, "complete -c tool -f\n").expect("completion file should exist");

        let report = execute_with_policy(
            &env,
            UninstallRequest {
                shell: Shell::Fish,
                program_name: "tool",
                path_override: Some(target.clone()),
            },
            ActivationPolicy::AutoManaged,
        )
        .expect("uninstall should succeed");

        assert_eq!(report.file_change, FileChange::Removed);
        assert_eq!(report.cleanup.mode, crate::ActivationMode::NativeDirectory);
        assert_eq!(report.cleanup.change, FileChange::Absent);
        assert!(!target.exists());
    }

    #[test]
    fn uninstall_with_manual_policy_and_explicit_default_fish_path_keeps_native_cleanup_mode() {
        let temp_root = crate::tests::temp_dir("uninstall-manual-explicit-default-fish-path");
        let home = temp_root.join("home");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_CONFIG_HOME")
            .without_real_path_lookups();
        let target = home.join(".config/fish/completions/tool.fish");
        fs::create_dir_all(target.parent().expect("default path should have a parent"))
            .expect("default completion dir should be creatable");
        fs::write(&target, "complete -c tool -f\n").expect("completion file should exist");

        let report = execute_with_policy(
            &env,
            UninstallRequest {
                shell: Shell::Fish,
                program_name: "tool",
                path_override: Some(target.clone()),
            },
            ActivationPolicy::Manual,
        )
        .expect("uninstall should succeed");

        assert_eq!(report.file_change, FileChange::Removed);
        assert_eq!(report.cleanup.mode, crate::ActivationMode::NativeDirectory);
        assert_eq!(report.cleanup.change, FileChange::Absent);
        assert!(!target.exists());
    }

    #[test]
    fn uninstall_rejects_relative_target_path_override() {
        let target = std::path::PathBuf::from("tool.bash");

        let error = execute(
            &Environment::test(),
            UninstallRequest {
                shell: Shell::Bash,
                program_name: "tool",
                path_override: Some(target.clone()),
            },
        )
        .expect_err("uninstall should fail");

        let report = crate::tests::assert_structural_failure(error, "uninstall");
        assert_eq!(report.kind, crate::FailureKind::InvalidTargetPath);
        assert_eq!(report.target_path, Some(target));
    }

    #[test]
    fn uninstall_rejects_relative_default_target_path_from_environment() {
        let env = Environment::test()
            .with_var("HOME", "relative-home")
            .with_var("XDG_DATA_HOME", "relative-cache")
            .without_real_path_lookups();

        let error = execute(
            &env,
            UninstallRequest {
                shell: Shell::Bash,
                program_name: "tool",
                path_override: None,
            },
        )
        .expect_err("uninstall should fail with invalid default path");

        let report = crate::tests::assert_structural_failure(error, "uninstall");
        assert_eq!(report.kind, crate::FailureKind::DefaultPathUnavailable);
        assert!(report.reason.contains("managed default completion path"));
    }

    #[cfg(unix)]
    #[test]
    fn uninstall_rejects_symlink_path_segments_in_target_override() {
        use std::os::unix::fs::symlink;

        let temp_root = crate::tests::temp_dir("uninstall-symlink-path");
        let real_dir = temp_root.join("real");
        let link_dir = temp_root.join("link");
        let target = link_dir.join("tool.bash");

        std::fs::create_dir_all(&real_dir).expect("real dir should be creatable");
        symlink(&real_dir, &link_dir).expect("symlink should be created");

        let error = execute(
            &Environment::test(),
            UninstallRequest {
                shell: Shell::Bash,
                program_name: "tool",
                path_override: Some(target.clone()),
            },
        )
        .expect_err("uninstall should reject symlink path");

        let report = crate::tests::assert_structural_failure(error, "uninstall");
        assert_eq!(report.kind, crate::FailureKind::InvalidTargetPath);
        assert_eq!(report.target_path, Some(target));
    }

    #[test]
    fn legacy_uninstall_with_explicit_default_fish_path_keeps_native_cleanup_mode() {
        let temp_root = crate::tests::temp_dir("uninstall-legacy-explicit-default-fish-path");
        let home = temp_root.join("home");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_CONFIG_HOME")
            .without_real_path_lookups();
        let target = home.join(".config/fish/completions/tool.fish");
        fs::create_dir_all(target.parent().expect("default path should have a parent"))
            .expect("default completion dir should be creatable");
        fs::write(&target, "complete -c tool -f\n").expect("completion file should exist");

        let report = execute(
            &env,
            UninstallRequest {
                shell: Shell::Fish,
                program_name: "tool",
                path_override: Some(target.clone()),
            },
        )
        .expect("uninstall should succeed");

        assert_eq!(report.file_change, FileChange::Removed);
        assert_eq!(report.cleanup.mode, crate::ActivationMode::NativeDirectory);
        assert_eq!(report.cleanup.change, FileChange::Absent);
        assert!(!target.exists());
    }

    #[test]
    fn uninstall_with_non_autoloadable_zsh_target_stays_manual() {
        let temp_root = crate::tests::temp_dir("uninstall-custom-zsh-manual");
        let target = temp_root.join("custom").join("tool.zsh");
        fs::create_dir_all(target.parent().expect("custom path should have parent"))
            .expect("custom dir should be creatable");
        fs::write(&target, "#compdef tool\n").expect("target file should exist");

        let report = execute_with_policy(
            &Environment::test().without_real_path_lookups(),
            UninstallRequest {
                shell: Shell::Zsh,
                program_name: "tool",
                path_override: Some(target.clone()),
            },
            ActivationPolicy::AutoManaged,
        )
        .expect("uninstall should succeed");

        assert_eq!(report.file_change, FileChange::Removed);
        assert_eq!(report.cleanup.mode, crate::ActivationMode::Manual);
        assert_eq!(report.cleanup.change, FileChange::Absent);
        assert!(!target.exists());
    }
}
