use std::path::Path;

use crate::error::{Error, Result};
use crate::infra::{
    env::Environment,
    fs,
    managed_block::{self, ManagedBlock},
};
use crate::model::{
    ActivationMode, ActivationReport, Availability, CleanupReport, FileChange, LegacyManagedBlock,
};
use crate::shell::{ActivationOutcome, CleanupOutcome, MigrationOutcome, migrate_profile_blocks};

pub(crate) fn install(
    env: &Environment,
    program_name: &str,
    target_path: &Path,
) -> Result<ActivationOutcome> {
    let profile_path = profile_path(env)?;
    let block = managed_block(program_name, target_path)?;
    managed_block::upsert(&profile_path, &block)?;

    Ok(ActivationOutcome {
        report: ActivationReport {
            mode: ActivationMode::ManagedRcBlock,
            availability: Availability::AvailableAfterNewShell,
            location: Some(profile_path.clone()),
            reason: Some(
                "shellcomp added a managed block to the PowerShell CurrentUserAllHosts profile."
                    .to_owned(),
            ),
            next_step: Some(format!(
                "Start a new PowerShell session or run `. {}`.",
                powershell_quote(&profile_path)?
            )),
        },
        affected_locations: vec![profile_path],
    })
}

pub(crate) fn uninstall(
    env: &Environment,
    program_name: &str,
    target_path: &Path,
) -> Result<CleanupOutcome> {
    let profile_path = profile_path(env)?;
    let block = managed_block(program_name, target_path)?;
    let profile_change = managed_block::remove(&profile_path, &block)?;

    Ok(CleanupOutcome {
        cleanup: CleanupReport {
            mode: ActivationMode::ManagedRcBlock,
            change: profile_change,
            location: Some(profile_path.clone()),
            reason: Some(match profile_change {
                FileChange::Removed => {
                    "Removed the managed PowerShell activation block from the CurrentUserAllHosts profile.".to_owned()
                }
                FileChange::Absent => {
                    "No managed PowerShell activation block was present in the CurrentUserAllHosts profile.".to_owned()
                }
                _ => "PowerShell activation cleanup completed.".to_owned(),
            }),
            next_step: None,
        },
        affected_locations: vec![profile_path],
    })
}

pub(crate) fn detect(
    env: &Environment,
    program_name: &str,
    target_path: &Path,
) -> Result<ActivationReport> {
    let installed = fs::file_exists(target_path)?;
    let profile_path = profile_path(env)?;
    let block = managed_block(program_name, target_path)?;
    let wired = managed_block::matches(&profile_path, &block)?;

    Ok(ActivationReport {
        mode: ActivationMode::ManagedRcBlock,
        availability: if !installed {
            Availability::ManualActionRequired
        } else if wired {
            Availability::AvailableAfterNewShell
        } else {
            Availability::ManualActionRequired
        },
        location: Some(if !installed && !wired {
            target_path.to_path_buf()
        } else {
            profile_path.clone()
        }),
        reason: Some(if !installed && wired {
            format!(
                "Managed PowerShell activation block is present in the CurrentUserAllHosts profile, but completion file `{}` is not installed.",
                target_path.display()
            )
        } else if !installed {
            format!(
                "Completion file `{}` is not installed.",
                target_path.display()
            )
        } else if wired {
            "Managed PowerShell activation block is present in the CurrentUserAllHosts profile."
                .to_owned()
        } else {
            "Completion file exists, but the managed PowerShell activation block was not found."
                .to_owned()
        }),
        next_step: Some(if !installed {
            "Run your CLI's completion install command or reinstall the PowerShell completion script."
                .to_owned()
        } else if wired {
            format!(
                "Start a new PowerShell session or run `. {}`.",
                powershell_quote(&profile_path)?
            )
        } else {
            format!(
                "Re-run installation, or add `. {}` to `$PROFILE.CurrentUserAllHosts` manually.",
                powershell_quote(target_path)?
            )
        }),
    })
}

pub(crate) fn migrate(
    env: &Environment,
    program_name: &str,
    target_path: &Path,
    legacy_blocks: &[LegacyManagedBlock],
) -> Result<MigrationOutcome> {
    let profile_path = profile_path(env)?;
    let block = managed_block(program_name, target_path)?;
    let (legacy_change, managed_change) =
        migrate_profile_blocks(&profile_path, legacy_blocks, &block)?;

    Ok(MigrationOutcome {
        location: Some(profile_path.clone()),
        managed_change,
        legacy_change,
        affected_locations: vec![profile_path],
    })
}

fn managed_block(program_name: &str, target_path: &Path) -> Result<ManagedBlock> {
    let quoted = powershell_quote(target_path)?;
    Ok(ManagedBlock {
        start_marker: format!("# >>> shellcomp powershell {program_name} >>>"),
        end_marker: format!("# <<< shellcomp powershell {program_name} <<<"),
        body: format!("if (Test-Path -LiteralPath {quoted} -PathType Leaf) {{\n  . {quoted}\n}}"),
    })
}

fn profile_path(env: &Environment) -> Result<std::path::PathBuf> {
    env.powershell_profile_path()
}

fn powershell_quote(path: &Path) -> Result<String> {
    let value = path.to_str().ok_or_else(|| Error::NonUtf8Path {
        path: path.to_path_buf(),
    })?;
    Ok(format!("'{}'", value.replace('\'', "''")))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{detect, install, migrate, profile_path, uninstall};
    use crate::infra::env::Environment;
    use crate::model::LegacyManagedBlock;
    use crate::model::{ActivationMode, Availability, FileChange};

    #[test]
    fn install_reports_managed_profile_activation() {
        let temp_root = crate::tests::temp_dir("powershell-install");
        let home = temp_root.join("home");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_CONFIG_HOME")
            .without_real_path_lookups();
        let target = home.join(".local/share/powershell/completions/tool.ps1");

        let report = install(&env, "tool", &target).expect("install should work");

        assert_eq!(report.report.mode, ActivationMode::ManagedRcBlock);
        assert_eq!(
            report.report.availability,
            Availability::AvailableAfterNewShell
        );
        assert!(
            report
                .report
                .next_step
                .as_deref()
                .is_some_and(|text| text.contains("profile.ps1"))
        );
    }

    #[test]
    fn install_next_step_uses_executable_dot_source_command() {
        let temp_root = crate::tests::temp_dir("powershell-install-next-step");
        let home = temp_root.join("home with space");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_CONFIG_HOME")
            .without_real_path_lookups();
        let target = home.join(".local/share/powershell/completions/tool.ps1");

        let report = install(&env, "tool", &target).expect("install should work");

        let next_step = report.report.next_step.expect("next_step should exist");
        assert!(next_step.contains("run `. '"));
        assert!(next_step.contains("home with space/.config/powershell/profile.ps1"));
    }

    #[test]
    fn detect_reports_managed_profile_when_script_exists() {
        let temp_root = crate::tests::temp_dir("powershell-detect");
        let home = temp_root.join("home");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_CONFIG_HOME")
            .without_real_path_lookups();
        let target = home.join(".local/share/powershell/completions/tool.ps1");
        fs::create_dir_all(target.parent().expect("target should have parent"))
            .expect("target dir should be creatable");
        fs::write(&target, "# powershell completion").expect("script should be writable");
        install(&env, "tool", &target).expect("install should work");

        let report = detect(&env, "tool", &target).expect("detect should work");

        assert_eq!(report.mode, ActivationMode::ManagedRcBlock);
        assert_eq!(report.availability, Availability::AvailableAfterNewShell);
        assert_eq!(
            report.location,
            Some(profile_path(&env).expect("profile path should resolve"))
        );
    }

    #[test]
    fn detect_requires_reinstall_when_profile_block_exists_but_script_is_missing() {
        let temp_root = crate::tests::temp_dir("powershell-detect-missing-script");
        let home = temp_root.join("home");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_CONFIG_HOME")
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();
        let target = home.join(".local/share/powershell/completions/tool.ps1");
        let profile = profile_path(&env).expect("profile path should resolve");
        std::fs::create_dir_all(profile.parent().expect("profile should have parent"))
            .expect("profile dir should be creatable");
        std::fs::write(
            &profile,
            "# >>> shellcomp powershell tool >>>\n$shellcompCompletion = '/tmp/tool.ps1'\nif (Test-Path -LiteralPath $shellcompCompletion -PathType Leaf) {\n  . $shellcompCompletion\n}\nRemove-Variable shellcompCompletion -ErrorAction SilentlyContinue\n# <<< shellcomp powershell tool <<<\n",
        )
        .expect("profile should be writable");

        let report = detect(&env, "tool", &target).expect("detect should work");

        assert_eq!(report.availability, Availability::ManualActionRequired);
        assert_eq!(report.location, Some(target));
        assert!(
            report
                .next_step
                .as_deref()
                .is_some_and(|text| text.contains("install command") || text.contains("reinstall"))
        );
    }

    #[test]
    fn detect_reports_actionable_guidance_when_profile_block_is_missing() {
        let temp_root = crate::tests::temp_dir("powershell-detect-unwired");
        let home = temp_root.join("home");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_CONFIG_HOME")
            .without_real_path_lookups();
        let target = home.join(".local/share/powershell/completions/tool.ps1");
        fs::create_dir_all(target.parent().expect("target should have parent"))
            .expect("target dir should be creatable");
        fs::write(&target, "# powershell completion").expect("script should be writable");

        let report = detect(&env, "tool", &target).expect("detect should work");

        assert_eq!(report.mode, ActivationMode::ManagedRcBlock);
        assert_eq!(report.availability, Availability::ManualActionRequired);
        assert_eq!(
            report.location,
            Some(profile_path(&env).expect("profile path should resolve"))
        );
        assert!(report.next_step.as_deref().is_some_and(|text| {
            text.contains("$PROFILE.CurrentUserAllHosts") && text.contains("tool.ps1")
        }));
    }

    #[test]
    fn profile_path_ignores_xdg_config_home_override() {
        let env = Environment::test()
            .with_var("HOME", "/tmp/home")
            .with_var("XDG_CONFIG_HOME", "/tmp/xdg-config");

        assert_eq!(
            profile_path(&env).expect("profile path should resolve"),
            std::path::PathBuf::from("/tmp/home/.config/powershell/profile.ps1")
        );
    }

    #[test]
    fn profile_path_uses_windows_documents_directory() {
        let env = Environment::test()
            .with_windows_platform()
            .with_var("USERPROFILE", r"C:\Users\demo")
            .without_var("HOME")
            .without_var("XDG_CONFIG_HOME");

        assert_eq!(
            profile_path(&env).expect("profile path should resolve"),
            std::path::PathBuf::from(r"C:\Users\demo")
                .join("Documents")
                .join("PowerShell")
                .join("profile.ps1")
        );
    }

    #[test]
    fn uninstall_reports_profile_cleanup() {
        let temp_root = crate::tests::temp_dir("powershell-uninstall");
        let home = temp_root.join("home");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_CONFIG_HOME")
            .without_real_path_lookups();
        let target = home.join(".local/share/powershell/completions/tool.ps1");
        install(&env, "tool", &target).expect("install should work");

        let report = uninstall(&env, "tool", &target).expect("uninstall should work");

        assert_eq!(report.cleanup.mode, ActivationMode::ManagedRcBlock);
        assert_eq!(report.cleanup.change, FileChange::Removed);
    }

    #[test]
    fn migrate_rewrites_legacy_profile_blocks() {
        let temp_root = crate::tests::temp_dir("powershell-migrate");
        let home = temp_root.join("home");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_CONFIG_HOME")
            .without_real_path_lookups();
        let profile = profile_path(&env).expect("profile path should resolve");
        let target = home.join(".local/share/powershell/completions/tool.ps1");
        std::fs::create_dir_all(profile.parent().expect("profile should have parent"))
            .expect("profile dir should be creatable");
        std::fs::write(
            &profile,
            "# >>> legacy tool >>>\n. '/tmp/tool.ps1'\n# <<< legacy tool <<<\n",
        )
        .expect("profile should be writable");

        let report = migrate(
            &env,
            "tool",
            &target,
            &[LegacyManagedBlock {
                start_marker: "# >>> legacy tool >>>".to_owned(),
                end_marker: "# <<< legacy tool <<<".to_owned(),
            }],
        )
        .expect("migration should work");

        assert_eq!(report.legacy_change, FileChange::Removed);
        assert_eq!(report.managed_change, FileChange::Created);
        let rendered = std::fs::read_to_string(profile).expect("profile should remain readable");
        assert!(rendered.contains("shellcomp powershell tool"));
        assert!(!rendered.contains("legacy tool"));
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::*;

    #[test]
    #[ignore = "requires pwsh; set SHELLCOMP_PWSH to its executable path"]
    fn generated_profile_loads_literal_paths_without_changing_user_variables() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("user's [profile] directory");
        std::fs::create_dir(&directory).unwrap();
        let target = directory.join("tool.ps1");
        std::fs::write(&target, "$global:shellcompTestLoaded = $true\n").unwrap();
        let block = managed_block("tool", &target).unwrap();
        for installed in [true, false] {
            if !installed {
                std::fs::remove_file(&target).unwrap();
            }
            let driver = root.path().join("test.ps1");
            std::fs::write(&driver, format!(
                "$ErrorActionPreference = 'Stop'\n$shellcompCompletion = 'user value'\n$global:shellcompTestLoaded = $false\n{}\nif ($global:shellcompTestLoaded -ne ${}) {{ throw 'wrong activation state' }}\nif ($shellcompCompletion -ne 'user value') {{ throw 'user variable was changed' }}\n",
                block.render(), if installed { "true" } else { "false" },
            )).unwrap();
            let output = std::process::Command::new(
                std::env::var_os("SHELLCOMP_PWSH").unwrap_or_else(|| "pwsh".into()),
            )
            .env("XDG_CACHE_HOME", root.path().join("cache"))
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("XDG_DATA_HOME", root.path().join("data"))
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
            .arg(driver)
            .output()
            .expect("PowerShell must be installed for this test");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
