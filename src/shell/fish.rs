use std::path::Path;

use crate::error::Result;
use crate::infra::fs;
use crate::model::{ActivationMode, ActivationReport, Availability, CleanupReport, FileChange};
use crate::shell::{ActivationOutcome, CleanupOutcome};

pub(crate) fn install(_program_name: &str, target_path: &Path) -> Result<ActivationOutcome> {
    Ok(ActivationOutcome {
        report: ActivationReport {
            mode: ActivationMode::NativeDirectory,
            availability: Availability::ActiveNow,
            location: Some(target_path.to_path_buf()),
            reason: Some("Installed into Fish's native completions directory.".to_owned()),
            next_step: None,
        },
        affected_locations: Vec::new(),
    })
}

pub(crate) fn uninstall(_program_name: &str, _target_path: &Path) -> Result<CleanupOutcome> {
    Ok(CleanupOutcome {
        cleanup: CleanupReport {
            mode: ActivationMode::NativeDirectory,
            change: FileChange::Absent,
            location: None,
            reason: Some(
                "Fish does not use a managed shell profile block for completion activation."
                    .to_owned(),
            ),
            next_step: None,
        },
        affected_locations: Vec::new(),
    })
}

pub(crate) fn detect(_program_name: &str, target_path: &Path) -> Result<ActivationReport> {
    let installed = fs::file_exists(target_path)?;

    Ok(ActivationReport {
        mode: ActivationMode::NativeDirectory,
        availability: if installed {
            Availability::ActiveNow
        } else {
            Availability::ManualActionRequired
        },
        location: Some(target_path.to_path_buf()),
        reason: Some(if installed {
            "Completion file is installed in Fish's native completions directory.".to_owned()
        } else {
            format!(
                "Completion file `{}` is not installed.",
                target_path.display()
            )
        }),
        next_step: if installed {
            None
        } else {
            Some(
                "Run your CLI's completion install command or place the completion file into Fish's completions directory."
                    .to_owned(),
            )
        },
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{detect, install, uninstall};
    use crate::model::{ActivationMode, Availability, FileChange};

    #[test]
    fn install_reports_native_directory_activation() {
        let report = install("tool", Path::new("/tmp/fish/completions/tool.fish"))
            .expect("install should succeed");

        assert_eq!(report.report.mode, ActivationMode::NativeDirectory);
        assert_eq!(report.report.availability, Availability::ActiveNow);
        assert!(report.report.next_step.is_none());
    }

    #[test]
    fn detect_reports_active_now_when_completion_exists() {
        let temp_root = crate::tests::temp_dir("fish-detect-installed");
        let target = temp_root.join("tool.fish");
        fs::write(&target, "complete -c tool\n").expect("completion file should be writable");

        let report = detect("tool", &target).expect("detect should succeed");

        assert_eq!(report.mode, ActivationMode::NativeDirectory);
        assert_eq!(report.availability, Availability::ActiveNow);
        assert_eq!(report.location, Some(target));
        assert!(report.next_step.is_none());
    }

    #[test]
    fn uninstall_reports_no_profile_cleanup() {
        let report = uninstall("tool", Path::new("/tmp/fish/completions/tool.fish"))
            .expect("uninstall should succeed");

        assert_eq!(report.cleanup.mode, ActivationMode::NativeDirectory);
        assert_eq!(report.cleanup.change, FileChange::Absent);
        assert!(report.cleanup.location.is_none());
    }
}
