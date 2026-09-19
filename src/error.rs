use std::error::Error as StdError;
use std::fmt::{Display, Formatter};
use std::io;
use std::path::{Path, PathBuf};

use crate::Shell;
use crate::model::FailureReport;

/// Convenience result type used by all public `shellcomp` APIs.
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
/// Errors returned by `shellcomp` operations.
///
/// Recoverable operational failures that callers are expected to render are wrapped as
/// [`Error::Failure`] with a [`crate::FailureReport`]. Lower-level variants represent immediate
/// input validation or filesystem problems.
pub enum Error {
    /// The requested program name was empty.
    EmptyProgramName,
    /// The requested program name contains unsupported path separators or reserved values.
    InvalidProgramName {
        /// The rejected program name.
        program_name: String,
    },
    /// `HOME` could not be resolved for an operation that requires it.
    MissingHome,
    /// The requested shell is known to the API but not implemented yet.
    UnsupportedShell(Shell),
    /// A target path did not contain a parent directory.
    PathHasNoParent {
        /// The invalid path.
        path: PathBuf,
    },
    /// A target path failed explicit validation for security or correctness reasons.
    InvalidTargetPath {
        /// The rejected path.
        path: PathBuf,
        /// Stable reason for failure classification.
        reason: &'static str,
    },
    /// A path could not be represented as UTF-8 for shell wiring purposes.
    NonUtf8Path {
        /// The path that could not be encoded.
        path: PathBuf,
    },
    /// A managed text file contained invalid UTF-8.
    InvalidUtf8File {
        /// The unreadable file path.
        path: PathBuf,
    },
    /// A managed block start marker was found without its matching end marker.
    ManagedBlockMissingEnd {
        /// The file containing the broken managed block.
        path: PathBuf,
        /// The expected start marker.
        start_marker: String,
        /// The expected end marker.
        end_marker: String,
    },
    /// Managed block markers or their arrangement are invalid.
    InvalidManagedBlock {
        /// The startup file that could not be safely processed.
        path: PathBuf,
        /// Description of the invalid markers or block structure.
        reason: &'static str,
    },
    /// A structured recoverable failure report intended for callers.
    ///
    /// Match this variant when you need stable failure kinds, affected paths, or caller-facing
    /// recovery guidance without parsing a display string.
    Failure {
        /// Structured operation status and recovery guidance.
        report: Box<FailureReport>,
        /// Original error that caused the operation to fail, when available.
        source: Option<Box<Error>>,
    },
    /// A filesystem operation failed.
    ///
    /// Most user-facing operational I/O failures are mapped to [`Error::Failure`] by the public
    /// APIs. This variant is still exposed because low-level helpers and validation paths may
    /// surface it directly.
    Io {
        /// The operation being attempted.
        action: &'static str,
        /// The path involved in the operation.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
}

impl Error {
    /// Returns a stable machine-readable error code.
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::EmptyProgramName => "shellcomp.empty_program_name",
            Self::InvalidProgramName { .. } => "shellcomp.invalid_program_name",
            Self::MissingHome => "shellcomp.missing_home",
            Self::UnsupportedShell(_) => "shellcomp.unsupported_shell",
            Self::PathHasNoParent { .. } => "shellcomp.invalid_target_path",
            Self::InvalidTargetPath { .. } => "shellcomp.invalid_target_path",
            Self::NonUtf8Path { .. } => "shellcomp.invalid_target_path",
            Self::InvalidUtf8File { .. } => "shellcomp.invalid_target_file",
            Self::ManagedBlockMissingEnd { .. } | Self::InvalidManagedBlock { .. } => {
                "shellcomp.profile_corrupted"
            }
            Self::Failure { report, .. } => report.error_code(),
            Self::Io { .. } => "shellcomp.io_error",
        }
    }

    /// Returns whether a retry may succeed with changed environment or timing.
    pub const fn is_retryable(&self) -> bool {
        match self {
            Self::Failure { report, .. } => report.is_retryable(),
            Self::Io { .. } => true,
            _ => false,
        }
    }

    /// Returns the operation-scoped trace id when this is a structured failure.
    pub fn trace_id(&self) -> Option<u64> {
        match self {
            Self::Failure { report, .. } => Some(report.trace_id),
            _ => None,
        }
    }

    pub(crate) fn io(action: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            action,
            path: path.into(),
            source,
        }
    }

    pub(crate) fn failure(report: FailureReport) -> Self {
        Self::Failure {
            report: Box::new(report),
            source: None,
        }
    }

    /// Add an operational report without discarding the original failure.
    pub(crate) fn with_context(self, describe: impl FnOnce(&Self) -> Option<Self>) -> Self {
        match describe(&self) {
            Some(Self::Failure { report, .. }) => Self::Failure {
                report,
                source: Some(Box::new(self)),
            },
            Some(error) => error,
            None => self,
        }
    }

    /// Returns `Some` when the error is [`Error::Failure`].
    pub fn as_failure(&self) -> Option<&FailureReport> {
        match self {
            Self::Failure { report, .. } => Some(report),
            _ => None,
        }
    }

    /// Converts a [`Error::Failure`] into a plain [`FailureReport`].
    ///
    /// This is useful when callers need stable, structured failure data and prefer not to
    /// branch on internals in each match arm. This conversion discards the source chain;
    /// keep the original error when its underlying cause is needed for diagnostics.
    pub fn into_failure(self) -> Option<FailureReport> {
        match self {
            Self::Failure { report, .. } => Some(*report),
            _ => None,
        }
    }

    /// Returns the most relevant filesystem location for this error, when one exists.
    ///
    /// For [`Error::Failure`], this returns the report's primary `target_path`. Use
    /// [`crate::FailureReport::affected_locations`] when you need the full set of related paths.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use shellcomp::Error;
    ///
    /// let error = Error::InvalidProgramName {
    ///     program_name: "bad/name".to_owned(),
    /// };
    ///
    /// assert_eq!(error.location(), None);
    /// ```
    pub fn location(&self) -> Option<&Path> {
        match self {
            Self::PathHasNoParent { path }
            | Self::InvalidTargetPath { path, .. }
            | Self::NonUtf8Path { path }
            | Self::InvalidUtf8File { path }
            | Self::Io { path, .. } => Some(path.as_path()),
            Self::ManagedBlockMissingEnd { path, .. } | Self::InvalidManagedBlock { path, .. } => {
                Some(path.as_path())
            }
            Self::Failure { report, .. } => report.target_path.as_deref(),
            Self::EmptyProgramName
            | Self::InvalidProgramName { .. }
            | Self::MissingHome
            | Self::UnsupportedShell(_) => None,
        }
    }

    /// Returns a human-readable failure reason intended for caller-side rendering.
    ///
    /// This is suitable for logs or CLI output, but callers that need stable branching should
    /// prefer matching [`Error::Failure`] and reading [`crate::FailureReport::kind`].
    ///
    /// # Examples
    ///
    /// ```rust
    /// use shellcomp::Error;
    ///
    /// let error = Error::MissingHome;
    /// assert!(error.reason().is_some());
    /// ```
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Failure { report, .. } => Some(report.reason.as_str()),
            Self::EmptyProgramName => Some("Program name must not be empty."),
            Self::InvalidProgramName { .. } => Some(
                "Program names must use a safe portable character set: ASCII letters, digits, `.`, `_`, and `-`.",
            ),
            Self::MissingHome => Some(
                "The operation requires a user home directory because the default shell-managed path could not be resolved.",
            ),
            Self::UnsupportedShell(_) => Some(
                "This shell is modelled in the API but not implemented in the production support set yet.",
            ),
            Self::PathHasNoParent { .. } => {
                Some("The provided path does not have a parent directory.")
            }
            Self::InvalidTargetPath { reason, .. } | Self::InvalidManagedBlock { reason, .. } => {
                Some(reason)
            }
            Self::NonUtf8Path { .. } => Some(
                "The path cannot be represented safely in shell startup wiring because it is not valid UTF-8.",
            ),
            Self::InvalidUtf8File { .. } => Some(
                "The managed file could not be parsed as UTF-8, so shellcomp cannot safely update it.",
            ),
            Self::ManagedBlockMissingEnd { .. } => {
                Some("A managed shell block is malformed because its closing marker is missing.")
            }
            Self::Io { .. } => None,
        }
    }

    /// Returns a suggested next step for this error, when one exists.
    ///
    /// This is primarily intended for CLI or UI layers that want to surface actionable guidance
    /// without inventing shell-specific recovery text themselves.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use shellcomp::Error;
    ///
    /// let error = Error::PathHasNoParent {
    ///     path: "/".into(),
    /// };
    ///
    /// assert!(error.next_step().is_some());
    /// ```
    pub fn next_step(&self) -> Option<&str> {
        match self {
            Self::Failure { report, .. } => report.next_step.as_deref(),
            Self::MissingHome => Some(
                "Set HOME or the relevant shell-specific home variable for the current process, or pass `path_override` so the library does not need a default managed path.",
            ),
            Self::PathHasNoParent { .. } => Some(
                "Pass a file path with a real parent directory, or create the parent directory before calling shellcomp.",
            ),
            Self::InvalidTargetPath { reason, .. } if *reason == "target path must be absolute" => {
                Some("Pass an absolute path so shellcomp can apply safe path validation reliably.")
            }
            Self::InvalidTargetPath { reason, .. }
                if *reason == "target path must not be a symbolic link" =>
            {
                Some(
                    "Choose a path in a non-symlink directory and avoid symlink completion targets.",
                )
            }
            Self::InvalidTargetPath { reason, .. } => Some(match *reason {
                "target path must be normalized" => {
                    "Pass a normalized absolute path without `.` or `..` segments."
                }
                "target path parent must be an existing directory" => {
                    "Create the parent directory before calling shellcomp."
                }
                _ => "Use an explicit non-relative, non-symlink target path.",
            }),
            Self::InvalidProgramName { .. } => Some(
                "Rename the binary or pass a sanitized program name that only uses ASCII letters, digits, `.`, `_`, and `-`.",
            ),
            _ => None,
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyProgramName => write!(f, "program name must not be empty"),
            Self::InvalidProgramName { program_name } => {
                write!(
                    f,
                    "program name `{program_name}` contains unsupported characters"
                )
            }
            Self::MissingHome => write!(
                f,
                "no supported home-directory environment variable is set and no fallback path can be resolved"
            ),
            Self::UnsupportedShell(shell) => write!(f, "shell `{shell}` is not supported yet"),
            Self::PathHasNoParent { path } => {
                write!(
                    f,
                    "path `{}` does not have a parent directory",
                    path.display()
                )
            }
            Self::InvalidTargetPath { path, reason } => {
                write!(f, "target path `{}` is invalid: {reason}", path.display())
            }
            Self::NonUtf8Path { path } => {
                write!(
                    f,
                    "path `{}` cannot be represented as UTF-8",
                    path.display()
                )
            }
            Self::InvalidUtf8File { path } => {
                write!(f, "file `{}` is not valid UTF-8", path.display())
            }
            Self::ManagedBlockMissingEnd {
                path,
                start_marker,
                end_marker,
            } => write!(
                f,
                "managed block `{start_marker}` in `{}` is missing closing marker `{end_marker}`",
                path.display()
            ),
            Self::InvalidManagedBlock { path, reason } => {
                write!(f, "invalid managed block in `{}`: {reason}", path.display())
            }
            Self::Failure { report, .. } => write!(f, "{}", report.reason),
            Self::Io {
                action,
                path,
                source,
            } => write!(f, "failed to {action} `{}`: {source}", path.display()),
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Failure { source, .. } => source
                .as_deref()
                .map(|error| error as &(dyn StdError + 'static)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::Error;
    use crate::model::{
        ActivationMode, ActivationReport, Availability, FailureKind, FailureReport, Operation,
        Shell,
    };

    #[test]
    fn failure_helpers_forward_report_context() {
        let error = Error::failure(FailureReport {
            operation: Operation::Install,
            shell: Shell::Bash,
            target_path: Some(PathBuf::from("/tmp/tool")),
            affected_locations: vec![PathBuf::from("/tmp/tool"), PathBuf::from("/tmp/.bashrc")],
            kind: FailureKind::ProfileUnavailable,
            file_change: Some(crate::FileChange::Created),
            activation: Some(ActivationReport {
                mode: ActivationMode::Manual,
                availability: Availability::ManualActionRequired,
                location: Some(PathBuf::from("/tmp/.bashrc")),
                reason: Some("profile update failed".to_owned()),
                next_step: Some("edit your shell profile manually".to_owned()),
            }),
            cleanup: None,
            reason: "Could not update the managed Bash startup block.".to_owned(),
            next_step: Some("edit your shell profile manually".to_owned()),
            trace_id: 123,
        });

        assert_eq!(error.location(), Some(PathBuf::from("/tmp/tool").as_path()));
        assert_eq!(
            error.reason(),
            Some("Could not update the managed Bash startup block.")
        );
        assert_eq!(error.next_step(), Some("edit your shell profile manually"));
        assert_eq!(error.as_failure().unwrap().trace_id, 123);
    }

    #[test]
    fn builtin_error_helpers_return_actionable_context() {
        let error = Error::InvalidProgramName {
            program_name: "bad/name".to_owned(),
        };

        assert_eq!(
            error.reason(),
            Some(
                "Program names must use a safe portable character set: ASCII letters, digits, `.`, `_`, and `-`."
            )
        );
        assert_eq!(
            error.next_step(),
            Some(
                "Rename the binary or pass a sanitized program name that only uses ASCII letters, digits, `.`, `_`, and `-`."
            )
        );
        assert_eq!(error.location(), None);
    }

    #[test]
    fn error_helpers_expose_stable_code_retryability_and_trace() {
        let report = FailureReport {
            operation: Operation::Install,
            shell: Shell::Bash,
            target_path: Some(PathBuf::from("/tmp/tool")),
            affected_locations: vec![PathBuf::from("/tmp/tool")],
            kind: FailureKind::CompletionFileUnreadable,
            file_change: None,
            activation: None,
            cleanup: None,
            reason: "write failure".to_owned(),
            next_step: Some("retry after fixing permissions".to_owned()),
            trace_id: 99,
        };

        let error = Error::failure(report);

        assert_eq!(
            error.error_code(),
            FailureKind::CompletionFileUnreadable.code()
        );
        assert!(error.is_retryable());
        assert_eq!(error.trace_id(), Some(99));

        let invalid_path = Error::InvalidTargetPath {
            path: PathBuf::from("relative"),
            reason: "target path must be absolute",
        };

        assert_eq!(invalid_path.error_code(), "shellcomp.invalid_target_path");
        assert!(!invalid_path.is_retryable());
        assert_eq!(invalid_path.trace_id(), None);
    }

    #[test]
    fn missing_home_helpers_use_generic_home_directory_guidance() {
        let error = Error::MissingHome;

        assert_eq!(
            error.reason(),
            Some(
                "The operation requires a user home directory because the default shell-managed path could not be resolved."
            )
        );
        assert_eq!(
            error.next_step(),
            Some(
                "Set HOME or the relevant shell-specific home variable for the current process, or pass `path_override` so the library does not need a default managed path."
            )
        );
        assert_eq!(
            error.to_string(),
            "no supported home-directory environment variable is set and no fallback path can be resolved"
        );
    }
}
