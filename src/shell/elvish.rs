use std::path::Path;

use crate::error::Result;
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
    _env: &Environment,
    program_name: &str,
    target_path: &Path,
) -> Result<ActivationOutcome> {
    let profile_path = rc_path(_env)?;
    let block = managed_block(program_name, target_path)?;
    managed_block::upsert(&profile_path, &block)?;

    Ok(ActivationOutcome {
        report: ActivationReport {
            mode: ActivationMode::ManagedRcBlock,
            availability: Availability::AvailableAfterNewShell,
            location: Some(profile_path.clone()),
            reason: Some("shellcomp added a managed block to rc.elv.".to_owned()),
            next_step: Some(format!(
                "Start a new Elvish session or run `eval (slurp < {})`.",
                elvish_quote(&profile_path)?
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
    let profile_path = rc_path(env)?;
    let block = managed_block(program_name, target_path)?;
    let profile_change = managed_block::remove(&profile_path, &block)?;

    Ok(CleanupOutcome {
        cleanup: CleanupReport {
            mode: ActivationMode::ManagedRcBlock,
            change: profile_change,
            location: Some(profile_path.clone()),
            reason: Some(match profile_change {
                FileChange::Removed => {
                    "Removed the managed Elvish activation block from rc.elv.".to_owned()
                }
                FileChange::Absent => {
                    "No managed Elvish activation block was present in rc.elv.".to_owned()
                }
                _ => "Elvish activation cleanup completed.".to_owned(),
            }),
            next_step: None,
        },
        affected_locations: vec![profile_path],
    })
}

pub(crate) fn detect(
    _env: &Environment,
    program_name: &str,
    target_path: &Path,
) -> Result<ActivationReport> {
    let installed = fs::file_exists(target_path)?;
    let profile_path = rc_path(_env)?;
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
                "Managed Elvish activation block is present in rc.elv, but completion file `{}` is not installed.",
                target_path.display()
            )
        } else if !installed {
            format!(
                "Completion file `{}` is not installed.",
                target_path.display()
            )
        } else if wired {
            "Managed Elvish activation block is present in rc.elv.".to_owned()
        } else {
            "Completion file exists, but the managed Elvish activation block was not found."
                .to_owned()
        }),
        next_step: Some(if !installed {
            "Run your CLI's completion install command or reinstall the Elvish completion script."
                .to_owned()
        } else if wired {
            format!(
                "Start a new Elvish session or run `eval (slurp < {})`.",
                elvish_quote(&profile_path)?
            )
        } else {
            format!(
                "Re-run installation, or add `eval (slurp < {})` to your Elvish rc.elv manually.",
                elvish_quote(target_path)?
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
    let profile_path = rc_path(env)?;
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
    let quoted = elvish_quote(target_path)?;
    Ok(ManagedBlock {
        start_marker: format!("# >>> shellcomp elvish {program_name} >>>"),
        end_marker: format!("# <<< shellcomp elvish {program_name} <<<"),
        body: format!("use os\nif (os:exists {quoted}) {{\n  eval (slurp < {quoted})\n}}"),
    })
}

fn elvish_quote(path: &Path) -> Result<String> {
    let value = path
        .to_str()
        .ok_or_else(|| crate::error::Error::NonUtf8Path {
            path: path.to_path_buf(),
        })?;
    Ok(format!("'{}'", value.replace('\'', "''")))
}

fn rc_path(env: &Environment) -> Result<std::path::PathBuf> {
    Ok(env.xdg_config_home()?.join("elvish").join("rc.elv"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{detect, install, managed_block, migrate, rc_path, uninstall};
    use crate::infra::env::Environment;
    use crate::model::{ActivationMode, Availability, FileChange, LegacyManagedBlock};
    use std::path::Path;

    #[test]
    fn install_reports_managed_rc_guidance() {
        let temp_root = crate::tests::temp_dir("elvish-install");
        let home = temp_root.join("home");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_CONFIG_HOME")
            .without_real_path_lookups();

        let report = install(
            &env,
            "tool",
            &home.join(".config/elvish/lib/shellcomp/tool.elv"),
        )
        .expect("install should work");

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
                .is_some_and(|text| text.contains("rc.elv"))
        );
    }

    #[test]
    fn install_next_step_uses_executable_eval_command_for_rc_path() {
        let temp_root = crate::tests::temp_dir("elvish-install-next-step");
        let home = temp_root.join("home with space");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_CONFIG_HOME")
            .without_real_path_lookups();
        let target = home.join(".config/elvish/lib/shellcomp/tool.elv");

        let report = install(&env, "tool", &target).expect("install should work");

        let next_step = report.report.next_step.expect("next_step should exist");
        assert!(next_step.contains("eval (slurp < '"));
        assert!(next_step.contains("home with space/.config/elvish/rc.elv"));
    }

    #[test]
    fn managed_block_imports_os_module_before_using_os_exists() {
        let block = managed_block(
            "tool",
            Path::new("/tmp/home/.config/elvish/lib/shellcomp/tool.elv"),
        )
        .expect("block should be valid");

        assert!(block.body.starts_with("use os\n"));
        assert!(block.body.contains("if (os:exists"));
    }

    #[test]
    fn detect_reports_managed_when_script_exists() {
        let temp_root = crate::tests::temp_dir("elvish-detect");
        let home = temp_root.join("home");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_CONFIG_HOME")
            .without_real_path_lookups();
        let target = home.join(".config/elvish/lib/shellcomp/tool.elv");
        fs::create_dir_all(target.parent().expect("target should have a parent"))
            .expect("target dir should be creatable");
        fs::write(&target, "# elvish completion").expect("script should be writable");
        install(&env, "tool", &target).expect("install should work");

        let report = detect(&env, "tool", &target).expect("detect should work");

        assert_eq!(report.mode, ActivationMode::ManagedRcBlock);
        assert_eq!(report.availability, Availability::AvailableAfterNewShell);
        assert_eq!(
            report.location,
            Some(rc_path(&env).expect("rc path should resolve"))
        );
    }

    #[test]
    fn detect_reports_actionable_guidance_when_rc_block_is_missing() {
        let temp_root = crate::tests::temp_dir("elvish-detect-unwired");
        let home = temp_root.join("home with space");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_CONFIG_HOME")
            .without_real_path_lookups();
        let target = home.join(".config/elvish/lib/shellcomp/tool.elv");
        fs::create_dir_all(target.parent().expect("target should have a parent"))
            .expect("target dir should be creatable");
        fs::write(&target, "# elvish completion").expect("script should be writable");

        let report = detect(&env, "tool", &target).expect("detect should work");

        assert_eq!(report.mode, ActivationMode::ManagedRcBlock);
        assert_eq!(report.availability, Availability::ManualActionRequired);
        assert_eq!(
            report.location,
            Some(rc_path(&env).expect("rc path should resolve"))
        );
        assert!(report.next_step.as_deref().is_some_and(|text| {
            text.contains("eval (slurp < '") && text.contains("tool.elv") && text.contains("rc.elv")
        }));
    }

    #[test]
    fn detect_requires_reinstall_when_rc_block_exists_but_script_is_missing() {
        let temp_root = crate::tests::temp_dir("elvish-detect-missing-script");
        let home = temp_root.join("home");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_CONFIG_HOME")
            .without_real_path_lookups();
        let target = home.join(".config/elvish/lib/shellcomp/tool.elv");
        let rc = rc_path(&env).expect("rc path should resolve");
        std::fs::create_dir_all(rc.parent().expect("rc should have parent"))
            .expect("rc dir should be creatable");
        std::fs::write(
            &rc,
            "# >>> shellcomp elvish tool >>>\nif (os:exists '/tmp/tool.elv') {\n  eval (slurp < '/tmp/tool.elv')\n}\n# <<< shellcomp elvish tool <<<\n",
        )
        .expect("rc should be writable");

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
    fn uninstall_reports_rc_cleanup() {
        let temp_root = crate::tests::temp_dir("elvish-uninstall");
        let home = temp_root.join("home");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_CONFIG_HOME")
            .without_real_path_lookups();
        let target = home.join(".config/elvish/lib/shellcomp/tool.elv");
        install(&env, "tool", &target).expect("install should work");

        let report = uninstall(&env, "tool", &target).expect("uninstall should work");

        assert_eq!(report.cleanup.mode, ActivationMode::ManagedRcBlock);
        assert_eq!(report.cleanup.change, FileChange::Removed);
    }

    #[test]
    fn migrate_rewrites_legacy_rc_blocks() {
        let temp_root = crate::tests::temp_dir("elvish-migrate");
        let home = temp_root.join("home");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_CONFIG_HOME")
            .without_real_path_lookups();
        let rc = rc_path(&env).expect("rc path should resolve");
        let target = home.join(".config/elvish/lib/shellcomp/tool.elv");
        std::fs::create_dir_all(rc.parent().expect("rc should have parent"))
            .expect("rc dir should be creatable");
        std::fs::write(
            &rc,
            "# >>> legacy tool >>>\neval (slurp < '/tmp/tool.elv')\n# <<< legacy tool <<<\n",
        )
        .expect("rc should be writable");

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
        let rendered = std::fs::read_to_string(rc).expect("rc should remain readable");
        assert!(rendered.contains("shellcomp elvish tool"));
        assert!(!rendered.contains("legacy tool"));
    }
}
