use std::path::Path;

use crate::error::{Error, Result};
use crate::infra::{env::Environment, fs, paths};
use crate::model::{
    ActivationMode, ActivationPolicy, ActivationReport, Availability, FailureKind, FileChange,
    InstallReport, InstallRequest, Operation, Shell,
};
use crate::service::{
    FailureContext, FailureStatus, default_target_path_matches, failure, failure_with_status,
    home_env_hint, legacy_activation_policy, manual_activation_report, push_unique,
    resolve_target_path, should_use_shell_backend, with_operation_lock, with_operation_observation,
};
use crate::shell;

pub(crate) fn execute(env: &Environment, request: InstallRequest<'_>) -> Result<InstallReport> {
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
    request: InstallRequest<'_>,
    activation_policy: ActivationPolicy,
) -> Result<InstallReport> {
    let shell = request.shell.clone();
    let program_name = request.program_name;
    with_operation_observation(
        Operation::Install,
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
                let file_change = fs::write_if_changed(&target_path, request.script)
                    .map_err(|error| map_write_error(&request, &target_path, error))?;

                let mut affected_locations = Vec::new();
                push_unique(&mut affected_locations, target_path.clone());

                let activation = if should_use_shell_backend(
                    env,
                    &request.shell,
                    program_name,
                    activation_policy,
                    &target_path,
                ) {
                    let outcome = shell::install_default(
                        env,
                        &request.shell,
                        request.program_name,
                        &target_path,
                    )
                    .map_err(|error| {
                        map_activation_error(env, &request.shell, &target_path, file_change, error)
                    })?;
                    for path in outcome.affected_locations {
                        push_unique(&mut affected_locations, path);
                    }
                    outcome.report
                } else {
                    manual_policy_activation(
                        env,
                        &request,
                        activation_policy,
                        &target_path,
                        file_change,
                    )?
                };

                Ok(InstallReport {
                    shell: request.shell,
                    target_path,
                    file_change,
                    activation,
                    affected_locations,
                })
            })
        },
        |report| Some(report.target_path.clone()),
    )
}

fn manual_policy_activation(
    env: &Environment,
    request: &InstallRequest<'_>,
    activation_policy: ActivationPolicy,
    target_path: &Path,
    file_change: FileChange,
) -> Result<ActivationReport> {
    if default_target_path_matches(env, &request.shell, request.program_name, target_path) {
        let detected =
            match shell::detect_default(env, &request.shell, request.program_name, target_path) {
                Ok(report) => Some(report),
                Err(_) if matches!(activation_policy, ActivationPolicy::Manual) => None,
                Err(error) => {
                    return Err(map_activation_error(
                        env,
                        &request.shell,
                        target_path,
                        file_change,
                        error,
                    ));
                }
            };

        if let Some(detected) = detected
            && detected.availability != Availability::ManualActionRequired
        {
            return Ok(detected);
        }
    }

    manual_activation_report(
        &request.shell,
        request.program_name,
        target_path,
        request.path_override.is_some(),
        activation_policy,
    )
    .map_err(|error| map_activation_error(env, &request.shell, target_path, file_change, error))
}

fn map_resolve_error(env: &Environment, request: &InstallRequest<'_>, error: Error) -> Error {
    error.with_context(|error| {
        let uses_default_target = request.path_override.is_none();

        Some(match error {
            Error::MissingHome => failure(
                FailureContext {
                    operation: Operation::Install,
                    shell: &request.shell,
                    target_path: request.path_override.as_deref(),
                    affected_locations: Vec::new(),
                    kind: FailureKind::MissingHome,
                },
                format!(
                    "Could not resolve the default managed install path because {} is not set.",
                    home_env_hint(env, &request.shell)
                ),
                Some(
                    format!(
                        "Set {} for the current process or pass `path_override` to install into an explicit path.",
                        home_env_hint(env, &request.shell)
                    ),
                ),
            ),
            Error::PathHasNoParent { path } => failure(
                FailureContext {
                    operation: Operation::Install,
                    shell: &request.shell,
                    target_path: Some(path),
                    affected_locations: vec![path.clone()],
                    kind: FailureKind::InvalidTargetPath,
                },
                format!(
                    "The requested install path `{}` does not have a parent directory.",
                    path.display()
                ),
                Some(
                    "Pass a file path with a real parent directory, or create the parent directory first."
                    .to_owned(),
                ),
            ),
            Error::InvalidTargetPath { path, reason } => failure(
                FailureContext {
                    operation: Operation::Install,
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
                    format!("The managed default install path `{}` is invalid: {reason}.", path.display())
                } else {
                    format!(
                        "The requested install path `{}` is invalid: {reason}.",
                        path.display()
                    )
                },
                Some(
                    "Choose an absolute, non-symlink, normalized custom target path with an existing parent directory."
                        .to_owned(),
                ),
            ),
            Error::UnsupportedShell(shell) => failure(
                FailureContext {
                    operation: Operation::Install,
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

fn map_write_error(request: &InstallRequest<'_>, target_path: &Path, error: Error) -> Error {
    error.with_context(|error| {
        Some(match error {
            Error::Io { action, .. } => failure(
                FailureContext {
                    operation: Operation::Install,
                    shell: &request.shell,
                    target_path: Some(target_path),
                    affected_locations: vec![target_path.to_path_buf()],
                    kind: match *action {
                        "read file" => FailureKind::CompletionFileUnreadable,
                        _ => FailureKind::CompletionTargetUnavailable,
                    },
                },
                format!(
                    "Could not write the {} completion file to `{}`.",
                    request.shell,
                    target_path.display()
                ),
                Some(default_write_next_step(
                    &request.shell,
                    target_path,
                    request.path_override.is_some(),
                )),
            ),
            Error::InvalidUtf8File { path } => failure(
                FailureContext {
                    operation: Operation::Install,
                    shell: &request.shell,
                    target_path: Some(target_path),
                    affected_locations: vec![path.clone(), target_path.to_path_buf()],
                    kind: FailureKind::CompletionFileUnreadable,
                },
                format!(
                    "The existing completion file `{}` could not be read as UTF-8, so shellcomp could not update it safely.",
                    path.display()
                ),
                Some(default_write_next_step(
                    &request.shell,
                    target_path,
                    request.path_override.is_some(),
                )),
            ),
            _ => return None,
        })
    })
}

fn default_write_next_step(shell: &Shell, target_path: &Path, has_override: bool) -> String {
    match shell {
        Shell::Bash => {
            if has_override {
                format!(
                    "Choose a writable custom path, then source {} manually from a Bash startup file.",
                    sh_single_quote(target_path)
                )
            } else {
                format!(
                    "Choose a writable path with `path_override`, or install the completion file manually at {}.",
                    sh_single_quote(target_path)
                )
            }
        }
        Shell::Zsh => {
            let completion_dir = target_path
                .parent()
                .map_or_else(|| sh_single_quote(target_path), sh_single_quote);
            if has_override {
                format!(
                    "Choose a writable custom path, then add {} to `fpath` and run `compinit -i` manually.",
                    completion_dir
                )
            } else {
                format!(
                    "Choose a writable path with `path_override`, or place the completion file at {} and ensure {} is on `fpath`.",
                    sh_single_quote(target_path),
                    completion_dir
                )
            }
        }
        Shell::Fish => {
            if has_override {
                format!(
                    "Choose a writable custom path, then place or source {} manually in Fish.",
                    sh_single_quote(target_path)
                )
            } else {
                format!(
                    "Choose a writable path with `path_override`, or place the completion file manually at {}.",
                    sh_single_quote(target_path)
                )
            }
        }
        Shell::Powershell => {
            format!(
                "Choose a writable path, then add `. {}` to a PowerShell profile.",
                powershell_quote(target_path)
            )
        }
        Shell::Elvish => {
            format!(
                "Choose a writable path, then add `eval (slurp < {})` to your Elvish rc.elv.",
                elvish_quote(target_path)
            )
        }
        Shell::Other(_) => {
            format!(
                "Choose a writable path and activate the completion file at `{}` manually.",
                target_path.display()
            )
        }
    }
}

fn map_activation_error(
    env: &Environment,
    shell: &Shell,
    target_path: &Path,
    file_change: FileChange,
    error: Error,
) -> Error {
    error.with_context(|error| {
        let startup_path = error.location().map(Path::to_path_buf);
        if let Error::NonUtf8Path { path } = error {
            return Some(failure_with_status(
                FailureContext {
                    operation: Operation::Install,
                    shell,
                    target_path: Some(target_path),
                    affected_locations: vec![target_path.to_path_buf(), path.clone()],
                    kind: FailureKind::InvalidTargetPath,
                },
                FailureStatus {
                    file_change: Some(file_change),
                    activation: Some(ActivationReport {
                        mode: ActivationMode::Manual,
                        availability: Availability::ManualActionRequired,
                        location: Some(target_path.to_path_buf()),
                        reason: Some(
                            "The completion file was written, but shellcomp could not represent its path safely in shell activation wiring."
                                .to_owned(),
                        ),
                        next_step: Some(
                            "Move the completion file to a UTF-8 path and run install again so shellcomp can manage activation safely."
                                .to_owned(),
                        ),
                    }),
                    cleanup: None,
                },
                "The completion file was written, but shellcomp could not represent its path safely in shell activation wiring.",
                Some(
                    "Move the completion file to a UTF-8 path and run install again so shellcomp can manage activation safely."
                        .to_owned(),
                ),
            ));
        }

        let (reason, next_step) = match shell {
            Shell::Bash => (
                "Could not update the managed Bash startup block.".to_owned(),
                format!(
                    "Source {} manually from a writable Bash startup file, or use `path_override` and handle activation yourself.",
                    sh_single_quote(target_path)
                ),
            ),
            Shell::Zsh => (
                "Could not update the managed Zsh startup block.".to_owned(),
                format!(
                    "Add {} to `fpath` manually and run `compinit -i`, or use `path_override` and handle activation yourself.",
                    target_path
                        .parent()
                        .map_or_else(|| sh_single_quote(target_path), sh_single_quote)
                ),
            ),
            Shell::Powershell => (
                "Could not update the managed PowerShell profile block.".to_owned(),
                format!(
                    "Add `. {}` to a PowerShell profile manually, or use `path_override` and handle activation yourself.",
                    powershell_quote(target_path)
                ),
            ),
            Shell::Elvish => (
                "Could not update the managed Elvish rc.elv block.".to_owned(),
                format!(
                    "Add `eval (slurp < {})` to rc.elv manually, or use `path_override` and handle activation yourself.",
                    elvish_quote(target_path)
                ),
            ),
            _ => return None,
        };

        let activation = ActivationReport {
            mode: ActivationMode::Manual,
            availability: Availability::ManualActionRequired,
            location: error.location().map(Path::to_path_buf),
            reason: Some(reason.clone()),
            next_step: Some(next_step.clone()),
        };

        Some(match error {
            Error::MissingHome => failure_with_status(
                FailureContext {
                    operation: Operation::Install,
                    shell,
                    target_path: Some(target_path),
                    affected_locations: vec![target_path.to_path_buf()],
                    kind: FailureKind::MissingHome,
                },
                FailureStatus {
                    file_change: Some(file_change),
                    activation: Some(activation),
                    cleanup: None,
                },
                format!(
                    "Could not resolve the managed {} startup file because {} is not set.",
                    shell,
                    home_env_hint(env, shell)
                ),
                Some(format!(
                    "Set {} for the current process so shellcomp can resolve the managed startup file, or {}",
                    home_env_hint(env, shell),
                    next_step,
                )),
            ),
            Error::Io { path, .. }
            | Error::InvalidUtf8File { path }
            | Error::InvalidTargetPath { path, .. } => failure_with_status(
                FailureContext {
                    operation: Operation::Install,
                    shell,
                    target_path: Some(target_path),
                    affected_locations: vec![target_path.to_path_buf(), path.clone()],
                    kind: FailureKind::ProfileUnavailable,
                },
                FailureStatus {
                    file_change: Some(file_change),
                    activation: Some(activation),
                    cleanup: None,
                },
                reason,
                Some(match startup_path.as_deref() {
                    Some(path) => format!("Review {} manually, or {}", path.display(), next_step),
                    None => next_step,
                }),
            ),
            Error::ManagedBlockMissingEnd { path, .. } | Error::InvalidManagedBlock { path, .. } => {
                failure_with_status(
                    FailureContext {
                        operation: Operation::Install,
                        shell,
                        target_path: Some(target_path),
                        affected_locations: vec![target_path.to_path_buf(), path.clone()],
                        kind: FailureKind::ProfileCorrupted,
                    },
                    FailureStatus {
                        file_change: Some(file_change),
                        activation: Some(activation),
                        cleanup: None,
                    },
                    reason,
                    Some(match startup_path.as_deref() {
                        Some(path) => format!(
                            "Repair or remove the malformed block in {} manually, or {}",
                            path.display(),
                            next_step
                        ),
                        None => next_step,
                    }),
                )
            }
            _ => return None,
        })
    })
}

fn sh_single_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
}

fn powershell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

fn elvish_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{elvish_quote, execute, execute_with_policy, powershell_quote};
    use crate::infra::env::Environment;
    use crate::model::{
        ActivationMode, ActivationPolicy, Availability, FileChange, InstallRequest, Operation,
        Shell,
    };

    #[test]
    fn install_with_path_override_requires_manual_activation() {
        let temp_root = crate::tests::temp_dir("install-path-override");
        let target = temp_root.join("custom").join("tool.bash");
        let env = Environment::test();

        let report = execute(
            &env,
            InstallRequest {
                shell: Shell::Bash,
                program_name: "tool",
                script: b"complete -F _tool tool\n",
                path_override: Some(target.clone()),
            },
        )
        .expect("install should succeed");

        assert_eq!(report.file_change, FileChange::Created);
        assert_eq!(report.activation.mode, ActivationMode::Manual);
        assert_eq!(
            report.activation.availability,
            Availability::ManualActionRequired
        );
        assert_eq!(
            fs::read(&target).expect("target file should exist"),
            b"complete -F _tool tool\n"
        );
    }

    #[test]
    fn install_bash_uses_managed_rc_block_without_loader() {
        let temp_root = crate::tests::temp_dir("install-bash-managed");
        let home = temp_root.join("home");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();

        let report = execute(
            &env,
            InstallRequest {
                shell: Shell::Bash,
                program_name: "tool",
                script: b"complete -F _tool tool\n",
                path_override: None,
            },
        )
        .expect("install should succeed");

        assert_eq!(report.file_change, FileChange::Created);
        assert_eq!(report.activation.mode, ActivationMode::ManagedRcBlock);
        assert_eq!(
            report.activation.availability,
            Availability::AvailableAfterSource
        );
        let bashrc = home.join(".bashrc");
        let bashrc_contents = fs::read_to_string(bashrc).expect(".bashrc should be created");
        assert!(bashrc_contents.contains("shellcomp bash tool"));
        assert!(bashrc_contents.contains(". '"));
    }

    #[test]
    fn install_returns_missing_home_when_default_path_cannot_resolve() {
        let env = Environment::test()
            .without_var("HOME")
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();

        let error = execute(
            &env,
            InstallRequest {
                shell: Shell::Bash,
                program_name: "tool",
                script: b"complete -F _tool tool\n",
                path_override: None,
            },
        )
        .expect_err("install should fail without HOME");

        assert!(matches!(
            error,
            crate::Error::Failure { report, .. } if report.kind == crate::FailureKind::MissingHome
        ));
    }

    #[test]
    fn install_rejects_relative_default_target_path_from_environment() {
        let env = Environment::test()
            .with_var("HOME", "relative-home")
            .with_var("XDG_DATA_HOME", "relative-cache")
            .without_real_path_lookups();

        let error = execute(
            &env,
            InstallRequest {
                shell: Shell::Bash,
                program_name: "tool",
                script: b"complete -F _tool tool\n",
                path_override: None,
            },
        )
        .expect_err("install should fail with invalid default path");

        let report = crate::tests::assert_structural_failure(error, "install");
        assert_eq!(report.kind, crate::FailureKind::DefaultPathUnavailable);
        assert!(report.reason.contains("managed default install path"));
    }

    #[test]
    fn install_reports_userprofile_hint_for_windows_powershell_activation_failure() {
        let temp_root = crate::tests::temp_dir("install-powershell-windows-missing-home");
        let target = temp_root.join("custom").join("tool.ps1");
        let env = Environment::test()
            .with_windows_platform()
            .without_var("HOME")
            .without_var("USERPROFILE")
            .without_real_path_lookups();

        let error = execute_with_policy(
            &env,
            InstallRequest {
                shell: Shell::Powershell,
                program_name: "tool",
                script: b"# powershell completion\n",
                path_override: Some(target),
            },
            ActivationPolicy::AutoManaged,
        )
        .expect_err("install should fail structurally");

        let report = crate::tests::assert_structural_failure(error, "install");
        assert_eq!(report.kind, crate::FailureKind::MissingHome);
        assert!(report.reason.contains("HOME or USERPROFILE is not set"));
        assert!(
            report
                .next_step
                .as_deref()
                .is_some_and(|text| text.contains("HOME or USERPROFILE"))
        );
    }

    #[test]
    fn install_fails_structurally_when_managed_bash_profile_is_not_writable() {
        let temp_root = crate::tests::temp_dir("install-bash-actionable");
        let home = temp_root.join("home");
        let bashrc = home.join(".bashrc");
        fs::create_dir_all(&home).expect("home should be creatable");
        fs::create_dir_all(&bashrc).expect(".bashrc directory should be creatable");

        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();

        let error = execute(
            &env,
            InstallRequest {
                shell: Shell::Bash,
                program_name: "tool",
                script: b"complete -F _tool tool\n",
                path_override: None,
            },
        )
        .expect_err("install should fail structurally");

        let report = crate::tests::assert_structural_failure(error, "install");
        assert_eq!(report.kind, crate::FailureKind::ProfileUnavailable);
        assert_eq!(report.file_change, Some(FileChange::Created));
        let activation = report.activation.expect("activation context should exist");
        assert_eq!(activation.mode, ActivationMode::Manual);
        assert_eq!(activation.availability, Availability::ManualActionRequired);
        assert!(
            report
                .affected_locations
                .iter()
                .any(|path| path.ends_with(".bashrc"))
        );
        assert!(
            report
                .next_step
                .as_deref()
                .is_some_and(|text| text.contains(&bashrc.display().to_string()))
        );
    }

    #[test]
    fn install_fails_structurally_when_managed_zsh_profile_is_not_writable() {
        let temp_root = crate::tests::temp_dir("install-zsh-structural-failure");
        let home = temp_root.join("home");
        let zshrc = home.join(".zshrc");
        fs::create_dir_all(&home).expect("home should be creatable");
        fs::create_dir_all(&zshrc).expect(".zshrc directory should be creatable");

        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("ZDOTDIR")
            .without_real_path_lookups();

        let error = execute(
            &env,
            InstallRequest {
                shell: Shell::Zsh,
                program_name: "tool",
                script: b"#compdef tool\n",
                path_override: None,
            },
        )
        .expect_err("install should fail structurally");

        let report = crate::tests::assert_structural_failure(error, "install");
        assert_eq!(report.kind, crate::FailureKind::ProfileUnavailable);
        assert_eq!(report.file_change, Some(FileChange::Created));
        let activation = report.activation.expect("activation context should exist");
        assert_eq!(activation.mode, ActivationMode::Manual);
        assert_eq!(activation.availability, Availability::ManualActionRequired);
        assert!(
            report
                .affected_locations
                .iter()
                .any(|path| path.ends_with(".zshrc"))
        );
        assert!(
            report
                .next_step
                .as_deref()
                .is_some_and(|text| text.contains(&zshrc.display().to_string()))
        );
    }

    #[test]
    fn install_failure_quotes_bash_manual_guidance_for_paths_with_spaces() {
        let temp_root = crate::tests::temp_dir("install-bash-failure-guidance-spaces");
        let home = temp_root.join("home with space");
        fs::create_dir_all(&home).expect("home should be creatable");
        fs::create_dir_all(home.join(".bashrc")).expect(".bashrc directory should be creatable");

        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();

        let error = execute(
            &env,
            InstallRequest {
                shell: Shell::Bash,
                program_name: "tool",
                script: b"complete -F _tool tool\n",
                path_override: None,
            },
        )
        .expect_err("install should fail structurally");

        let report = crate::tests::assert_structural_failure(error, "install");
        let next_step = report.next_step.expect("next_step should exist");
        assert!(next_step.contains("Source '"));
        assert!(
            next_step.contains("home with space/.local/share/bash-completion/completions/tool")
        );
    }

    #[test]
    fn install_failure_quotes_powershell_manual_guidance_for_paths_with_spaces() {
        let temp_root = crate::tests::temp_dir("install-powershell-failure-guidance-spaces");
        let home = temp_root.join("home with space");
        fs::create_dir_all(home.join(".config/powershell"))
            .expect("profile parent should be creatable");
        fs::create_dir_all(home.join(".config/powershell/profile.ps1"))
            .expect("profile path should be a directory");

        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();

        let error = execute(
            &env,
            InstallRequest {
                shell: Shell::Powershell,
                program_name: "tool",
                script: b"# powershell completion\n",
                path_override: None,
            },
        )
        .expect_err("install should fail structurally");

        let report = crate::tests::assert_structural_failure(error, "install");
        let next_step = report.next_step.expect("next_step should exist");
        assert!(next_step.contains(". '"));
        assert!(next_step.contains("home with space/.local/share/powershell/completions/tool.ps1"));
    }

    #[test]
    fn install_failure_quotes_zsh_manual_guidance_for_paths_with_spaces() {
        let temp_root = crate::tests::temp_dir("install-zsh-failure-guidance-spaces");
        let home = temp_root.join("home");
        let zdotdir = temp_root.join("zdot dir");
        fs::create_dir_all(&home).expect("home should be creatable");
        fs::create_dir_all(zdotdir.join(".zshrc")).expect(".zshrc path should be a directory");

        let env = Environment::test()
            .with_var("HOME", &home)
            .with_var("ZDOTDIR", &zdotdir)
            .without_real_path_lookups();

        let error = execute(
            &env,
            InstallRequest {
                shell: Shell::Zsh,
                program_name: "tool",
                script: b"#compdef tool\n",
                path_override: None,
            },
        )
        .expect_err("install should fail structurally");

        let report = crate::tests::assert_structural_failure(error, "install");
        let next_step = report.next_step.expect("next_step should exist");
        assert!(next_step.contains("'"));
        assert!(next_step.contains("zdot dir/.zfunc"));
        assert!(next_step.contains("fpath"));
    }

    #[test]
    fn install_failure_uses_executable_elvish_manual_guidance() {
        let temp_root = crate::tests::temp_dir("install-elvish-failure-guidance-spaces");
        let home = temp_root.join("home with space");
        fs::create_dir_all(home.join(".config/elvish")).expect("rc parent should be creatable");
        fs::create_dir_all(home.join(".config/elvish/rc.elv"))
            .expect("rc path should be a directory");

        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_CONFIG_HOME")
            .without_real_path_lookups();

        let error = execute(
            &env,
            InstallRequest {
                shell: Shell::Elvish,
                program_name: "tool",
                script: b"# elvish completion\n",
                path_override: None,
            },
        )
        .expect_err("install should fail structurally");

        let report = crate::tests::assert_structural_failure(error, "install");
        let next_step = report.next_step.expect("next_step should exist");
        assert!(next_step.contains("eval (slurp < '"));
        assert!(next_step.contains("home with space/.config/elvish/lib/shellcomp/tool.elv"));
    }

    #[test]
    fn install_write_failure_uses_executable_powershell_default_guidance() {
        let temp_root = crate::tests::temp_dir("install-powershell-write-failure-guidance");
        let target = temp_root.join("target with space").join("tool.ps1");
        fs::create_dir_all(&target).expect("target path should be a directory");
        let env = Environment::test();

        let error = execute(
            &env,
            InstallRequest {
                shell: Shell::Powershell,
                program_name: "tool",
                script: b"# powershell completion\n",
                path_override: Some(target.clone()),
            },
        )
        .expect_err("install should fail on unreadable target");

        let report = crate::tests::assert_structural_failure(error, "install");
        assert_eq!(report.kind, crate::FailureKind::CompletionFileUnreadable);
        let next_step = report.next_step.expect("next_step should exist");
        assert!(next_step.contains(&format!(". {}", powershell_quote(&target))));
        assert!(!next_step.contains("<path>"));
    }

    #[test]
    fn install_write_failure_uses_executable_elvish_default_guidance() {
        let temp_root = crate::tests::temp_dir("install-elvish-write-failure-guidance");
        let target = temp_root.join("target with space").join("tool.elv");
        fs::create_dir_all(&target).expect("target path should be a directory");
        let env = Environment::test();

        let error = execute(
            &env,
            InstallRequest {
                shell: Shell::Elvish,
                program_name: "tool",
                script: b"# elvish completion\n",
                path_override: Some(target.clone()),
            },
        )
        .expect_err("install should fail on unreadable target");

        let report = crate::tests::assert_structural_failure(error, "install");
        assert_eq!(report.kind, crate::FailureKind::CompletionFileUnreadable);
        let next_step = report.next_step.expect("next_step should exist");
        assert!(next_step.contains(&format!("eval (slurp < {})", elvish_quote(&target))));
        assert!(!next_step.contains("<path>"));
    }

    #[test]
    fn install_returns_profile_corrupted_for_malformed_bash_block() {
        let temp_root = crate::tests::temp_dir("install-bash-corrupted");
        let home = temp_root.join("home");
        let bashrc = home.join(".bashrc");
        let completion_dir = home.join(".local/share/bash-completion/completions");
        fs::create_dir_all(&completion_dir).expect("completion dir should be creatable");
        fs::write(&bashrc, "# >>> shellcomp bash tool >>>\n. '/tmp/tool'\n")
            .expect(".bashrc should be writable");

        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();

        let error = execute(
            &env,
            InstallRequest {
                shell: Shell::Bash,
                program_name: "tool",
                script: b"complete -F _tool tool\n",
                path_override: None,
            },
        )
        .expect_err("install should fail");

        let report = crate::tests::assert_structural_failure(error, "install");
        assert_eq!(report.operation, Operation::Install);
        assert_eq!(report.kind, crate::FailureKind::ProfileCorrupted);
        assert_eq!(report.target_path, Some(completion_dir.join("tool")));
        assert_eq!(report.file_change, Some(FileChange::Created));
        let activation = report.activation.expect("activation context should exist");
        assert_eq!(activation.mode, ActivationMode::Manual);
        assert_eq!(activation.availability, Availability::ManualActionRequired);
        assert!(
            report
                .affected_locations
                .iter()
                .any(|path| path.ends_with(".bashrc"))
        );
        assert!(
            report
                .next_step
                .as_deref()
                .is_some_and(|text| text.contains(&bashrc.display().to_string()))
        );
    }

    #[test]
    fn install_returns_unchanged_when_completion_file_matches() {
        let temp_root = crate::tests::temp_dir("install-unchanged");
        let target = temp_root.join("custom").join("tool.bash");
        let env = Environment::test();
        let request = InstallRequest {
            shell: Shell::Bash,
            program_name: "tool",
            script: b"complete -F _tool tool\n",
            path_override: Some(target),
        };

        let first = execute(&env, request.clone()).expect("first install should succeed");
        let second = execute(&env, request).expect("second install should succeed");

        assert_eq!(first.file_change, FileChange::Created);
        assert_eq!(second.file_change, FileChange::Unchanged);
    }

    #[test]
    fn install_with_custom_path_can_opt_into_managed_bash_activation() {
        let temp_root = crate::tests::temp_dir("install-custom-bash-managed");
        let home = temp_root.join("home");
        let target = temp_root.join("custom").join("tool.bash");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();

        let report = execute_with_policy(
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

        assert_eq!(report.activation.mode, ActivationMode::ManagedRcBlock);
        assert_eq!(
            report.activation.availability,
            Availability::AvailableAfterSource
        );
        let bashrc = fs::read_to_string(home.join(".bashrc")).expect(".bashrc should exist");
        assert!(bashrc.contains(&target.display().to_string()));
    }

    #[test]
    fn install_powershell_default_path_returns_managed_activation() {
        let temp_root = crate::tests::temp_dir("install-powershell-default");
        let home = temp_root.join("home");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();

        let report = execute(
            &env,
            InstallRequest {
                shell: Shell::Powershell,
                program_name: "tool",
                script: b"# powershell completion\n",
                path_override: None,
            },
        )
        .expect("install should succeed");

        assert_eq!(report.activation.mode, ActivationMode::ManagedRcBlock);
        assert_eq!(
            report.activation.availability,
            Availability::AvailableAfterNewShell
        );
        assert!(report.activation.next_step.is_some());
    }

    #[test]
    fn install_with_custom_powershell_path_quotes_next_step_safely() {
        let temp_root = crate::tests::temp_dir("install-powershell-quoted-path");
        let target = temp_root.join("demo's-tool.ps1");
        let env = Environment::test();

        let report = execute(
            &env,
            InstallRequest {
                shell: Shell::Powershell,
                program_name: "tool",
                script: b"# powershell completion\n",
                path_override: Some(target),
            },
        )
        .expect("install should succeed");

        assert!(
            report
                .activation
                .next_step
                .as_deref()
                .is_some_and(|text| text.contains("demo''s-tool.ps1"))
        );
    }

    #[test]
    fn install_with_custom_fish_path_does_not_report_native_activation() {
        let temp_root = crate::tests::temp_dir("install-custom-fish-manual");
        let target = temp_root.join("custom").join("tool.fish");
        let env = Environment::test();

        let report = execute_with_policy(
            &env,
            InstallRequest {
                shell: Shell::Fish,
                program_name: "tool",
                script: b"complete -c tool -f\n",
                path_override: Some(target),
            },
            ActivationPolicy::AutoManaged,
        )
        .expect("install should succeed");

        assert_eq!(report.activation.mode, ActivationMode::Manual);
        assert_eq!(
            report.activation.availability,
            Availability::ManualActionRequired
        );
    }

    #[test]
    fn install_with_manual_policy_keeps_default_fish_path_native() {
        let temp_root = crate::tests::temp_dir("install-default-fish-manual-policy");
        let home = temp_root.join("home");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_CONFIG_HOME")
            .without_real_path_lookups();

        let report = execute_with_policy(
            &env,
            InstallRequest {
                shell: Shell::Fish,
                program_name: "tool",
                script: b"complete -c tool -f\n",
                path_override: None,
            },
            ActivationPolicy::Manual,
        )
        .expect("install should succeed");

        assert_eq!(report.activation.mode, ActivationMode::NativeDirectory);
        assert_eq!(report.activation.availability, Availability::ActiveNow);
    }

    #[test]
    fn install_with_manual_policy_ignores_malformed_default_bash_profile() {
        let temp_root = crate::tests::temp_dir("install-bash-manual-ignores-malformed-profile");
        let home = temp_root.join("home");
        fs::create_dir_all(&home).expect("home should be creatable");
        fs::write(
            home.join(".bashrc"),
            "# >>> shellcomp bash tool >>>\n. '/tmp/tool'\n",
        )
        .expect(".bashrc should be writable");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();

        let report = execute_with_policy(
            &env,
            InstallRequest {
                shell: Shell::Bash,
                program_name: "tool",
                script: b"complete -F _tool tool\n",
                path_override: None,
            },
            ActivationPolicy::Manual,
        )
        .expect("manual install should ignore malformed managed profile state");

        assert_eq!(report.activation.mode, ActivationMode::Manual);
        assert_eq!(
            report.activation.availability,
            Availability::ManualActionRequired
        );
    }

    #[test]
    fn install_with_explicit_default_fish_path_can_still_report_native_activation() {
        let temp_root = crate::tests::temp_dir("install-explicit-default-fish-path");
        let home = temp_root.join("home");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_CONFIG_HOME")
            .without_real_path_lookups();
        let target = home.join(".config/fish/completions/tool.fish");

        let report = execute_with_policy(
            &env,
            InstallRequest {
                shell: Shell::Fish,
                program_name: "tool",
                script: b"complete -c tool -f\n",
                path_override: Some(target),
            },
            ActivationPolicy::AutoManaged,
        )
        .expect("install should succeed");

        assert_eq!(report.activation.mode, ActivationMode::NativeDirectory);
        assert_eq!(report.activation.availability, Availability::ActiveNow);
    }

    #[test]
    fn legacy_install_with_explicit_default_fish_path_reports_native_activation() {
        let temp_root = crate::tests::temp_dir("install-legacy-explicit-default-fish-path");
        let home = temp_root.join("home");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_CONFIG_HOME")
            .without_real_path_lookups();
        let target = home.join(".config/fish/completions/tool.fish");

        let report = execute(
            &env,
            InstallRequest {
                shell: Shell::Fish,
                program_name: "tool",
                script: b"complete -c tool -f\n",
                path_override: Some(target),
            },
        )
        .expect("install should succeed");

        assert_eq!(report.activation.mode, ActivationMode::NativeDirectory);
        assert_eq!(report.activation.availability, Availability::ActiveNow);
    }

    #[test]
    fn install_with_non_autoloadable_zsh_target_falls_back_to_manual() {
        let temp_root = crate::tests::temp_dir("install-custom-zsh-manual");
        let home = temp_root.join("home");
        let target = temp_root.join("custom").join("tool.zsh");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("ZDOTDIR")
            .without_real_path_lookups();

        let report = execute_with_policy(
            &env,
            InstallRequest {
                shell: Shell::Zsh,
                program_name: "tool",
                script: b"#compdef tool\n",
                path_override: Some(target),
            },
            ActivationPolicy::AutoManaged,
        )
        .expect("install should succeed");

        assert_eq!(report.activation.mode, ActivationMode::Manual);
        assert_eq!(
            report.activation.availability,
            Availability::ManualActionRequired
        );
    }

    #[test]
    fn install_with_custom_bash_path_does_not_use_system_loader() {
        let temp_root = crate::tests::temp_dir("install-custom-bash-direct-source");
        let home = temp_root.join("home");
        let target = temp_root.join("custom").join("tool.bash");
        let env = Environment::test()
            .with_var("HOME", &home)
            .with_var("BASH_COMPLETION_VERSINFO", "2")
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();

        let report = execute_with_policy(
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

        assert_eq!(report.activation.mode, ActivationMode::ManagedRcBlock);
        assert_eq!(
            report.activation.availability,
            Availability::AvailableAfterSource
        );
        let bashrc = fs::read_to_string(home.join(".bashrc")).expect(".bashrc should exist");
        assert!(bashrc.contains(&target.display().to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn install_reports_structured_failure_when_non_utf8_path_breaks_activation() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let temp_root = crate::tests::temp_dir("install-non-utf8-path");
        let target = temp_root.join(OsString::from_vec(b"tool-\xff.bash".to_vec()));
        let env = Environment::test();

        let error = execute(
            &env,
            InstallRequest {
                shell: Shell::Bash,
                program_name: "tool",
                script: b"complete -F _tool tool\n",
                path_override: Some(target.clone()),
            },
        )
        .expect_err("install should fail structurally");

        let report = crate::tests::assert_structural_failure(error, "install");
        assert_eq!(report.kind, crate::FailureKind::InvalidTargetPath);
        assert_eq!(report.file_change, Some(FileChange::Created));
        assert_eq!(report.target_path, Some(target));
        assert!(report.next_step.is_some());
    }

    #[test]
    fn install_rejects_relative_target_path_override() {
        let target = std::path::PathBuf::from("custom/tool.bash");
        let env = Environment::test();

        let error = execute(
            &env,
            InstallRequest {
                shell: Shell::Bash,
                program_name: "tool",
                script: b"complete -F _tool tool\n",
                path_override: Some(target),
            },
        )
        .expect_err("install should fail structurally");

        let report = crate::tests::assert_structural_failure(error, "install");
        assert_eq!(report.kind, crate::FailureKind::InvalidTargetPath);
        assert!(report.target_path.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn install_rejects_symlink_path_segments_in_target_override() {
        use std::os::unix::fs::symlink;

        let temp_root = crate::tests::temp_dir("install-symlink-path");
        let real_dir = temp_root.join("real");
        let link_dir = temp_root.join("link");
        let target = link_dir.join("tool.bash");

        std::fs::create_dir_all(&real_dir).expect("real dir should be creatable");
        symlink(&real_dir, &link_dir).expect("symlink should be created");

        let env = Environment::test().without_real_path_lookups();

        let error = execute(
            &env,
            InstallRequest {
                shell: Shell::Bash,
                program_name: "tool",
                script: b"complete -F _tool tool\n",
                path_override: Some(target.clone()),
            },
        )
        .expect_err("install should reject symlinked paths");

        let report = crate::tests::assert_structural_failure(error, "install");
        assert_eq!(report.kind, crate::FailureKind::InvalidTargetPath);
        assert_eq!(report.target_path, Some(target));
    }
}
