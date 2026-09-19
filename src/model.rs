use std::fmt::{Display, Formatter};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// Supported shells for completion installation and detection.
///
/// Bash, Zsh, Fish, PowerShell, and Elvish are currently supported. [`Shell::Other`] remains the
/// explicit escape hatch for unsupported shells.
pub enum Shell {
    /// GNU Bash.
    Bash,
    /// Z shell.
    Zsh,
    /// Fish shell.
    Fish,
    /// Elvish shell.
    Elvish,
    /// PowerShell.
    Powershell,
    /// An escape hatch for shells not modelled explicitly yet.
    ///
    /// This variant exists so callers can keep shell selection in their own domain model even when
    /// `shellcomp` does not implement that shell yet.
    Other(String),
}

impl Display for Shell {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bash => write!(f, "bash"),
            Self::Zsh => write!(f, "zsh"),
            Self::Fish => write!(f, "fish"),
            Self::Elvish => write!(f, "elvish"),
            Self::Powershell => write!(f, "powershell"),
            Self::Other(value) => write!(f, "{value}"),
        }
    }
}

#[cfg(feature = "clap")]
impl From<::clap_complete::Shell> for Shell {
    fn from(shell: ::clap_complete::Shell) -> Self {
        match shell {
            ::clap_complete::Shell::Bash => Self::Bash,
            ::clap_complete::Shell::Elvish => Self::Elvish,
            ::clap_complete::Shell::Fish => Self::Fish,
            ::clap_complete::Shell::PowerShell => Self::Powershell,
            ::clap_complete::Shell::Zsh => Self::Zsh,
            other => Self::Other(other.to_string()),
        }
    }
}

#[cfg(feature = "clap")]
impl TryFrom<Shell> for ::clap_complete::Shell {
    type Error = crate::Error;

    fn try_from(shell: Shell) -> std::result::Result<Self, Self::Error> {
        match shell {
            Shell::Bash => Ok(Self::Bash),
            Shell::Elvish => Ok(Self::Elvish),
            Shell::Fish => Ok(Self::Fish),
            Shell::Powershell => Ok(Self::PowerShell),
            Shell::Zsh => Ok(Self::Zsh),
            Shell::Other(value) => Err(crate::Error::UnsupportedShell(Shell::Other(value))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Result of a file mutation.
///
/// This enum is used across install, uninstall, and managed shell profile updates so callers can
/// report exactly what changed.
pub enum FileChange {
    /// The target did not exist and was created.
    Created,
    /// The target existed and its content changed.
    Updated,
    /// The target already matched the requested content.
    Unchanged,
    /// The target existed and was removed.
    Removed,
    /// The target did not exist, so nothing changed.
    Absent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// How a completion becomes active for a shell.
///
/// This describes the wiring mechanism rather than the current state. Pair it with
/// [`Availability`] to understand whether the completion is already usable.
pub enum ActivationMode {
    /// Activation relies on a system-level shell completion loader.
    SystemLoader,
    /// Activation is managed through a controlled shell startup block.
    ManagedRcBlock,
    /// Activation relies on the shell's native completion directory.
    NativeDirectory,
    /// Activation must be wired manually by the caller or user.
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Whether `shellcomp` should try to manage activation wiring automatically.
///
/// This policy matters most when installing to a custom path. Legacy [`InstallRequest`] behavior
/// remains unchanged: custom paths default to [`ActivationPolicy::Manual`], while managed default
/// locations default to [`ActivationPolicy::AutoManaged`].
pub enum ActivationPolicy {
    /// Let `shellcomp` apply the shell's managed activation behavior when supported.
    ///
    /// For Bash, Zsh, PowerShell, and Elvish this may update managed startup files. Fish keeps
    /// using its native completions directory, and unsupported or incompatible targets may still
    /// fall back to a manual activation report.
    AutoManaged,
    /// Write or remove the completion file without attempting managed activation wiring.
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Availability state for an installed completion.
///
/// This represents the best state `shellcomp` can determine from the current environment and
/// managed files.
pub enum Availability {
    /// The completion should be usable immediately.
    ActiveNow,
    /// The completion should be available after opening a new shell.
    AvailableAfterNewShell,
    /// The completion should be available after sourcing the affected startup file.
    AvailableAfterSource,
    /// Manual action is still required before the completion can work.
    ManualActionRequired,
    /// The library could not determine current availability.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// High-level operation associated with a structured failure.
pub enum Operation {
    /// Install a completion script and its activation wiring.
    Install,
    /// Detect activation status for a completion.
    DetectActivation,
    /// Rewrite or adopt managed shell startup blocks during migration.
    MigrateManagedBlocks,
    /// Remove a completion script and its activation wiring.
    Uninstall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// High-level lifecycle phase for operation-level observability hooks.
pub enum OperationEventPhase {
    /// The operation started.
    Started,
    /// The operation finished successfully.
    Succeeded,
    /// The operation finished with an error.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Structured event emitted for operation-level observability hooks.
pub struct OperationEvent {
    /// Operation being executed.
    pub operation: Operation,
    /// Event lifecycle phase.
    pub phase: OperationEventPhase,
    /// Target shell.
    pub shell: Shell,
    /// Requested program name.
    pub program_name: String,
    /// Scoped operation identifier visible in structured failures.
    pub trace_id: u64,
    /// Primary completion path involved in the operation, when available.
    pub target_path: Option<PathBuf>,
    /// Stable machine-readable code when the operation ended in failure.
    pub error_code: Option<&'static str>,
    /// Whether a retry is expected to help for this failure.
    pub retryable: bool,
    /// Elapsed duration in milliseconds from operation start for terminal phases.
    ///
    /// This field is omitted for `Started` events and populated for `Succeeded` /
    /// `Failed` events.
    pub duration_ms: Option<u128>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Stable failure kinds for recoverable operational errors.
///
/// [`crate::Error::Failure`] wraps a [`FailureReport`] carrying one of these kinds so callers can
/// branch on failure categories without parsing human-readable text.
pub enum FailureKind {
    /// `HOME` was not available and no fallback path could be derived.
    MissingHome,
    /// The requested shell is not implemented.
    UnsupportedShell,
    /// The requested target path was invalid.
    ///
    /// This typically means a provided `path_override` did not include a usable parent directory.
    InvalidTargetPath,
    /// The default managed install path could not be resolved.
    DefaultPathUnavailable,
    /// The completion file or its directory could not be created or written.
    CompletionTargetUnavailable,
    /// The completion file existed but could not be read as expected.
    CompletionFileUnreadable,
    /// The managed shell profile could not be written or removed.
    ProfileUnavailable,
    /// The managed shell profile was present but malformed.
    ///
    /// This usually means a previously managed block has a missing end marker or otherwise cannot
    /// be updated safely.
    ProfileCorrupted,
}

impl FailureKind {
    /// Returns a stable machine-readable code for monitoring and telemetry.
    pub const fn code(self) -> &'static str {
        match self {
            Self::MissingHome => "shellcomp.missing_home",
            Self::UnsupportedShell => "shellcomp.unsupported_shell",
            Self::InvalidTargetPath => "shellcomp.invalid_target_path",
            Self::DefaultPathUnavailable => "shellcomp.default_path_unavailable",
            Self::CompletionTargetUnavailable => "shellcomp.completion_target_unavailable",
            Self::CompletionFileUnreadable => "shellcomp.completion_file_unreadable",
            Self::ProfileUnavailable => "shellcomp.profile_unavailable",
            Self::ProfileCorrupted => "shellcomp.profile_corrupted",
        }
    }

    /// Returns whether the kind is generally worth a retry with corrected environment or timing.
    pub const fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::CompletionTargetUnavailable
                | Self::CompletionFileUnreadable
                | Self::ProfileUnavailable
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Input for a completion install operation.
///
/// Use [`path_override`](InstallRequest::path_override) when you want to control the destination
/// explicitly. Non-default custom paths normally require caller-managed activation unless you opt
/// into [`crate::install_with_policy`] with [`ActivationPolicy::AutoManaged`].
pub struct InstallRequest<'a> {
    /// Target shell.
    pub shell: Shell,
    /// Binary name used by the completion script.
    ///
    /// The name must be non-empty and use a portable ASCII character set consisting of letters,
    /// digits, `.`, `_`, and `-`.
    pub program_name: &'a str,
    /// Completion script bytes to install.
    pub script: &'a [u8],
    /// Optional explicit install path instead of the managed default path.
    ///
    /// Legacy [`crate::install`] behavior treats non-default custom paths as manual activation.
    /// When this path exactly matches the shell's managed default location, the usual managed or
    /// native activation semantics are preserved instead.
    pub path_override: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Input for a completion uninstall operation.
///
/// Passing [`path_override`](UninstallRequest::path_override) removes that explicit file instead
/// of resolving the default managed path.
pub struct UninstallRequest<'a> {
    /// Target shell.
    pub shell: Shell,
    /// Binary name used by the completion script.
    ///
    /// The same validation rules as [`InstallRequest::program_name`] apply.
    pub program_name: &'a str,
    /// Optional custom install path to remove instead of the default managed path.
    ///
    /// Legacy [`crate::uninstall`] behavior treats non-default custom paths as manual cleanup.
    /// When this path exactly matches the shell's managed default location, the usual managed or
    /// native cleanup semantics are preserved instead.
    pub path_override: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Caller-provided marker pair for a legacy managed shell block.
///
/// Use this with [`crate::migrate_managed_blocks`] when adopting `shellcomp` in a CLI that
/// previously managed its own shell startup block markers.
/// Markers must be distinct, nonempty single lines and match complete lines in the file.
/// Nested, overlapping, or unbalanced blocks are rejected before the file is modified.
pub struct LegacyManagedBlock {
    /// Start marker that uniquely identifies the legacy managed block.
    pub start_marker: String,
    /// Matching end marker for the legacy managed block.
    pub end_marker: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Input for managed-block migration into `shellcomp` markers.
///
/// This API rewrites a shell startup file to remove known legacy markers and upsert the equivalent
/// `shellcomp` managed block for the same shell and completion target path.
pub struct MigrateManagedBlocksRequest<'a> {
    /// Target shell whose managed startup block should be adopted.
    pub shell: Shell,
    /// Binary name used by the completion script.
    pub program_name: &'a str,
    /// Optional explicit completion file path to adopt.
    ///
    /// When omitted, `shellcomp` resolves the shell's managed default completion path.
    pub path_override: Option<PathBuf>,
    /// Caller-provided legacy marker pairs to remove before writing the `shellcomp` block.
    pub legacy_blocks: Vec<LegacyManagedBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Structured result of a completion install operation.
///
/// The report is designed for caller-side rendering. `shellcomp` does not print user-facing output
/// directly.
pub struct InstallReport {
    /// Target shell.
    pub shell: Shell,
    /// Final script path that was written.
    pub target_path: PathBuf,
    /// Outcome of writing the completion file.
    ///
    /// This only describes the completion script file itself. Startup wiring state is described by
    /// [`InstallReport::activation`].
    pub file_change: FileChange,
    /// Activation details for the installed completion.
    pub activation: ActivationReport,
    /// Files touched or referenced while completing the operation.
    ///
    /// This includes [`InstallReport::target_path`] and may also include a managed shell startup
    /// file such as `~/.bashrc` or `~/.zshrc`.
    pub affected_locations: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Structured cleanup status for activation wiring removal.
///
/// This is returned as part of [`RemoveReport`] and also embedded in [`FailureReport`] when an
/// uninstall operation fails after partial cleanup work.
pub struct CleanupReport {
    /// Activation mechanism associated with the cleanup.
    pub mode: ActivationMode,
    /// Outcome of the cleanup operation.
    pub change: FileChange,
    /// Shell-specific location involved in cleanup.
    ///
    /// For managed Bash/Zsh cleanup this is typically the startup file path. For Fish or manual
    /// custom-path installs, it is often `None`.
    pub location: Option<PathBuf>,
    /// Human-readable cleanup context.
    pub reason: Option<String>,
    /// Suggested next step when cleanup could not be completed automatically.
    pub next_step: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Structured result of a completion uninstall operation.
///
/// File removal and activation cleanup are reported separately so callers can preserve partial
/// progress in their own UX or logs.
pub struct RemoveReport {
    /// Target shell.
    pub shell: Shell,
    /// Final script path that was removed or checked.
    pub target_path: PathBuf,
    /// Outcome of removing the completion file.
    pub file_change: FileChange,
    /// Structured activation cleanup result.
    pub cleanup: CleanupReport,
    /// Files touched or referenced while completing the operation.
    ///
    /// This always includes [`RemoveReport::target_path`] and may also include a managed startup
    /// file when shell wiring cleanup was attempted.
    pub affected_locations: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Structured result of migrating legacy managed startup blocks into `shellcomp`.
pub struct MigrateManagedBlocksReport {
    /// Target shell.
    pub shell: Shell,
    /// Completion file path associated with the migrated managed block.
    pub target_path: PathBuf,
    /// Startup file updated during migration, when the shell uses one.
    pub location: Option<PathBuf>,
    /// Outcome of removing any caller-provided legacy blocks.
    pub legacy_change: FileChange,
    /// Outcome of writing the `shellcomp` managed block.
    pub managed_change: FileChange,
    /// Files touched or inspected while completing migration.
    pub affected_locations: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Structured activation status for an installed completion.
///
/// Use this report to decide whether to tell the user "ready now", "open a new shell", "source
/// your rc file", or "finish setup manually".
///
/// # Examples
///
/// ```rust
/// use shellcomp::{ActivationMode, Availability};
///
/// let mode = ActivationMode::ManagedRcBlock;
/// let availability = Availability::AvailableAfterSource;
///
/// assert_eq!(mode, ActivationMode::ManagedRcBlock);
/// assert_eq!(availability, Availability::AvailableAfterSource);
/// ```
pub struct ActivationReport {
    /// Activation mechanism in use.
    pub mode: ActivationMode,
    /// Current or expected availability state.
    ///
    /// Interpret this together with [`ActivationReport::mode`]. For example,
    /// [`ActivationMode::ManagedRcBlock`] plus [`Availability::AvailableAfterSource`] means the
    /// completion wiring is present but the current shell session may still need `source ~/.bashrc`
    /// or `source ~/.zshrc`.
    pub availability: Availability,
    /// Shell-specific location related to activation.
    ///
    /// For system-loader or native-directory activation this is often the completion file path.
    /// For managed startup wiring it is typically the startup file path.
    pub location: Option<PathBuf>,
    /// Machine-readable operations use enums; this field carries human-readable context.
    pub reason: Option<String>,
    /// Suggested next step for callers to render directly or adapt.
    pub next_step: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Structured failure report for recoverable operational errors.
///
/// `shellcomp` uses this report to preserve partial state like completion file changes or manual
/// recovery instructions. It is wrapped by [`crate::Error::Failure`].
///
/// # Examples
///
/// ```rust
/// use std::path::PathBuf;
///
/// use shellcomp::{Error, FailureKind, InstallRequest, Shell, install};
///
/// let error = install(InstallRequest {
///     shell: Shell::Fish,
///     program_name: "demo",
///     script: b"complete -c demo\n",
///     path_override: Some(PathBuf::from("/")),
/// })
/// .expect_err("path without parent should fail");
///
/// let report = error
///     .as_failure()
///     .expect("path without parent should fail");
/// assert_eq!(report.kind, FailureKind::InvalidTargetPath);
/// assert!(report.reason.contains("does not have a parent directory"));
/// ```
pub struct FailureReport {
    /// Operation that failed.
    pub operation: Operation,
    /// Target shell.
    pub shell: Shell,
    /// Path involved in the failure when known.
    ///
    /// This is usually the completion file target path, not necessarily the shell startup file that
    /// caused the failure.
    pub target_path: Option<PathBuf>,
    /// Related locations inspected or mutated before the failure.
    ///
    /// This may include both the completion target path and shell-specific locations such as
    /// `~/.bashrc` or `~/.zshrc`.
    pub affected_locations: Vec<PathBuf>,
    /// Stable failure kind for programmatic branching.
    pub kind: FailureKind,
    /// Outcome of the completion file change before the failure occurred, when available.
    ///
    /// This is especially useful during install failures where the completion file may already have
    /// been written successfully before shell activation wiring failed.
    pub file_change: Option<FileChange>,
    /// Activation state or fallback guidance known at failure time, when available.
    ///
    /// For example, a failed Bash/Zsh install may still return a manual activation recommendation.
    pub activation: Option<ActivationReport>,
    /// Cleanup state known at failure time, when available.
    pub cleanup: Option<CleanupReport>,
    /// Human-readable failure reason.
    pub reason: String,
    /// Suggested next step for recovery or manual completion.
    pub next_step: Option<String>,
    /// Invocation-scoped id generated for error correlation.
    pub trace_id: u64,
}

impl FailureReport {
    /// Stable machine-readable error code for structured telemetry.
    pub const fn error_code(&self) -> &'static str {
        self.kind.code()
    }

    /// Returns whether the failure may succeed after environment correction or retry.
    pub const fn is_retryable(&self) -> bool {
        self.kind.is_retryable()
    }
}
