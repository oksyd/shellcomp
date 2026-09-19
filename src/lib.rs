//! `shellcomp` is a deployment layer for shell completion scripts in Rust CLI projects.
//!
//! It does **not** generate completions. Instead, it focuses on the operational work around
//! completions:
//!
//! - choosing managed default install paths,
//! - writing completion files idempotently,
//! - wiring shell startup files when necessary,
//! - detecting current activation state,
//! - removing managed files and managed startup blocks,
//! - returning structured reports and structured failures for caller-side rendering.
//!
//! Pair it with `clap_complete` or any other generator when you need to render the script bytes.
//!
//! # Scope And Non-Goals
//!
//! `shellcomp` intentionally keeps a narrow scope:
//!
//! - it is **not** a shell completion generator;
//! - it is **not** a generic shell configuration manager;
//! - it does **not** attempt to understand arbitrary user shell customizations;
//! - it only modifies completion files it was asked to manage and startup blocks it can identify
//!   with stable markers.
//!
//! This is why the crate can stay small while still being production-usable.
//!
//! # Supported Shells
//!
//! Production support currently covers [`Shell::Bash`], [`Shell::Zsh`], [`Shell::Fish`],
//! [`Shell::Powershell`], and [`Shell::Elvish`]. [`Shell::Other`] remains the explicit escape
//! hatch for unsupported shells.
//!
//! Managed behavior for the supported shells:
//!
//! - [`Shell::Bash`]: writes to the XDG data completion directory, then prefers a system
//!   `bash-completion` loader and falls back to a managed `~/.bashrc` block when no loader is
//!   detected.
//! - [`Shell::Zsh`]: writes `_binary-name` into the managed zsh completion directory and maintains
//!   a managed `~/.zshrc` block that updates `fpath` and runs `compinit -i` when needed.
//! - [`Shell::Fish`]: writes directly into Fish's native completions directory and does not manage
//!   a shell startup file.
//! - [`Shell::Powershell`]: writes a managed completion script file and maintains a managed
//!   `CurrentUserAllHosts` profile block.
//! - [`Shell::Elvish`]: writes a managed completion script file and maintains a managed `rc.elv`
//!   block.
//!
//! Bash loader discovery is static: it recognizes simple `source`/`.` commands and common
//! profile-directory sourcing loops. It does not execute startup files or evaluate arbitrary
//! shell expressions. Quoted text, comments, and child-process execution are not evidence that
//! the current shell loads a completion script.
//!
//! # Public API
//!
//! The crate is intentionally small:
//!
//! - [`default_install_path`] resolves the managed target path for a shell and binary name.
//! - [`install`] writes the completion file and returns an [`InstallReport`].
//! - [`install_with_policy`] lets callers opt into or out of managed activation explicitly.
//! - [`detect_activation`] inspects the managed setup and returns an [`ActivationReport`].
//! - [`detect_activation_at_path`] inspects activation for an explicit completion file path.
//! - [`uninstall`] removes the managed file and returns a [`RemoveReport`].
//! - [`uninstall_with_policy`] lets callers control whether activation cleanup should be managed.
//! - [`migrate_managed_blocks`] removes known legacy markers and rewrites the equivalent
//!   `shellcomp`-managed startup block.
//! - [`render_clap_completion`] is available behind the `clap` feature for users who want the
//!   crate to render completion bytes from `clap::CommandFactory`.
//! - [`render_clap_completion_from_command`] is available behind the `clap` feature for users who
//!   want to tweak a prebuilt `clap::Command` before rendering.
//! - [`clap_complete`] is re-exported behind the `clap` feature so callers can use
//!   `shellcomp::clap_complete::Shell` without pulling conflicting top-level `Shell` names into
//!   the same scope manually.
//!
//! # Reading Reports
//!
//! The public report types are designed so callers can render UX without parsing display strings:
//!
//! - [`InstallReport::file_change`] tells you whether the completion file was created, updated, or
//!   already matched.
//! - [`ActivationReport::mode`] tells you *how* the shell will load the completion.
//! - [`ActivationReport::availability`] tells you whether it is active now, available after a new
//!   shell, available after sourcing a file, or still requires manual work.
//! - [`ActivationPolicy`] lets callers choose whether installation to a custom path should still
//!   attempt managed shell wiring when the shell supports it.
//! - [`RemoveReport::cleanup`] separates startup wiring cleanup from completion file removal so a
//!   caller can preserve partial progress.
//! - [`FailureReport`] carries structured failure kind, relevant paths, and suggested next steps.
//!
//! # Typical Integration Flow
//!
//! 1. Render a completion script with your preferred generator.
//! 2. Call [`install`] to place the script into a managed location.
//! 3. Show the returned [`ActivationReport`] to the user.
//! 4. Optionally call [`detect_activation`] later to re-check availability.
//! 5. Call [`uninstall`] to remove both the completion file and any managed activation wiring.
//!
//! # Install Example
//!
//! ```no_run
//! use shellcomp::{InstallRequest, Shell, install};
//!
//! let script = b"complete -F _demo demo\n";
//! let report = install(InstallRequest {
//!     shell: Shell::Bash,
//!     program_name: "demo",
//!     script,
//!     path_override: None,
//! })?;
//!
//! println!("installed to {}", report.target_path.display());
//! println!("activation: {:?}", report.activation);
//! # Ok::<(), shellcomp::Error>(())
//! ```
//!
//! # clap Integration Example
//!
//! With the `clap` feature enabled, you can keep the generator-facing shell value under
//! `shellcomp::clap_complete::Shell` and convert it into [`Shell`] only when you call the
//! deployment API.
//!
//! ```no_run
//! # #[cfg(feature = "clap")]
//! # fn install_zsh_completion() -> shellcomp::Result<()> {
//! # use clap::Parser;
//! # use shellcomp::{InstallRequest, install, render_clap_completion};
//! #
//! # #[derive(Parser)]
//! # struct Cli {
//! #     #[arg(long)]
//! #     verbose: bool,
//! # }
//! #
//! # let generator_shell = shellcomp::clap_complete::Shell::Zsh;
//! # let script = render_clap_completion::<Cli>(generator_shell, "demo")?;
//! # let report = install(InstallRequest {
//! #     shell: generator_shell.into(),
//! #     program_name: "demo",
//! #     script: &script,
//! #     path_override: None,
//! # })?;
//! #
//! # println!("installed to {}", report.target_path.display());
//! # Ok::<(), shellcomp::Error>(())
//! # }
//! ```
//!
//! # Custom Path Example
//!
//! Passing [`InstallRequest::path_override`] usually tells `shellcomp` to skip startup wiring and
//! report manual activation explicitly. The one exception is an override that exactly matches the
//! shell's managed default path, which still keeps the default activation semantics.
//!
//! ```no_run
//! use std::path::PathBuf;
//!
//! use shellcomp::{ActivationMode, Availability, InstallRequest, Shell, install};
//!
//! let report = install(InstallRequest {
//!     shell: Shell::Fish,
//!     program_name: "demo",
//!     script: b"complete -c demo\n",
//!     path_override: Some(PathBuf::from("/tmp/demo.fish")),
//! })?;
//!
//! assert_eq!(report.activation.mode, ActivationMode::Manual);
//! assert_eq!(report.activation.availability, Availability::ManualActionRequired);
//! # Ok::<(), shellcomp::Error>(())
//! ```
//!
//! # Structured Error Handling
//!
//! High-level operational failures are returned as [`Error::Failure`] with a stable
//! [`FailureKind`]. That lets callers keep their own presentation layer while still branching on
//! machine-readable failure categories. The original cause is retained through
//! [`std::error::Error::source`], including the underlying [`std::io::Error`] and OS error code.
//! Pattern matches use `Error::Failure { report, source }`; [`Error::as_failure`] provides
//! convenient access when only the report is needed.
//!
//! ```rust
//! use std::path::PathBuf;
//!
//! use shellcomp::{Error, FailureKind, InstallRequest, Shell, install};
//!
//! let error = install(InstallRequest {
//!     shell: Shell::Fish,
//!     program_name: "demo",
//!     script: b"complete -c demo\n",
//!     path_override: Some(PathBuf::from("/")),
//! })
//! .expect_err("path without parent should fail structurally");
//!
//! let report = error
//!     .as_failure()
//!     .expect("path without parent should fail structurally");
//! assert_eq!(report.kind, FailureKind::InvalidTargetPath);
//! ```
//!
//! Not every error becomes [`Error::Failure`]: immediate validation problems like
//! [`Error::InvalidProgramName`] are returned directly.
//!
//! # Idempotency
//!
//! Repeating the same install or uninstall operation is expected to be safe:
//!
//! - identical completion file writes return [`FileChange::Unchanged`];
//! - repeated removals return [`FileChange::Absent`];
//! - managed startup blocks are updated in place and removed by stable markers.
//!
//! This makes the crate suitable for CLI commands that users may run multiple times.

//! # File Updates And Concurrency
//!
//! Completion scripts and startup files are replaced using a temporary file in the same directory.
//! Existing file permissions are retained; new files use private temporary-file permissions.
//! Changed content cannot replace a read-only file; identical content remains a no-op.
//! Replacement changes file identity and does not preserve hard-link relationships or extended
//! metadata such as ACLs. The containing directory must be writable.
//!
//! Updates to the same completion target or startup file are serialized within this process.
//! Activation detection waits for operations on its completion target to finish before inspecting
//! the completion file and startup wiring.
//! Callers must coordinate concurrent processes and external editors themselves. An install and
//! its startup-file update are separate changes; failures report any completed progress.
//!
//! Paths must be absolute and must not contain symlinks or parent-directory components.
//! Startup files follow the same policy as completion targets. A completion target cannot be the
//! shell's startup file. Path checks do not protect against another process replacing parent
//! directories during an operation; managed directories must be trusted.
//!

#![forbid(unsafe_code)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]

mod api;
mod error;
mod model;
mod service;

pub(crate) mod infra;
pub(crate) mod shell;

#[cfg(test)]
mod tests;

#[cfg(feature = "clap")]
#[cfg_attr(docsrs, doc(cfg(feature = "clap")))]
pub use api::render_clap_completion;
#[cfg(feature = "clap")]
#[cfg_attr(docsrs, doc(cfg(feature = "clap")))]
pub use api::render_clap_completion_from_command;
#[cfg(feature = "clap")]
#[cfg_attr(docsrs, doc(cfg(feature = "clap")))]
/// Re-exported `clap_complete` shell type for callers that want a namespaced integration surface.
///
/// This lets downstream code write `shellcomp::clap_complete::Shell` without importing a second
/// top-level `Shell` symbol.
pub mod clap_complete {
    pub use ::clap_complete::Shell;
}
pub use api::{
    default_install_path, detect_activation, detect_activation_at_path, install,
    install_with_policy, migrate_managed_blocks, uninstall, uninstall_with_policy,
    with_operation_events,
};
pub use error::{Error, Result};
pub use model::{
    ActivationMode, ActivationPolicy, ActivationReport, Availability, CleanupReport, FailureKind,
    FailureReport, FileChange, InstallReport, InstallRequest, LegacyManagedBlock,
    MigrateManagedBlocksReport, MigrateManagedBlocksRequest, Operation, OperationEvent,
    OperationEventPhase, RemoveReport, Shell, UninstallRequest,
};
