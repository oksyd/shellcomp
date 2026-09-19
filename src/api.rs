use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::infra::env::Environment;
use crate::model::{
    ActivationPolicy, InstallReport, InstallRequest, MigrateManagedBlocksReport,
    MigrateManagedBlocksRequest, OperationEvent, RemoveReport, Shell, UninstallRequest,
};
use crate::service::{
    detect, install, migrate, resolve_default_target_path, uninstall, with_operation_event_hook,
    with_operation_trace,
};

/// Returns the default managed install path for a shell and binary name.
///
/// The returned path follows the managed layout implemented by `shellcomp` for supported shells.
/// It validates `program_name` before constructing the path. Empty or relative
/// `XDG_DATA_HOME` and `XDG_CONFIG_HOME` values are ignored, using HOME-based defaults.
///
/// The concrete layout currently used by the production support set is:
///
/// - Bash: `$XDG_DATA_HOME/bash-completion/completions/<program>`
/// - Zsh: `$ZDOTDIR/.zfunc/_<program>`
/// - Fish: `$XDG_CONFIG_HOME/fish/completions/<program>.fish`
/// - PowerShell:
///   - Windows: `%USERPROFILE%\\Documents\\PowerShell\\Completions\\<program>.ps1`
///   - Non-Windows: `$XDG_DATA_HOME/powershell/completions/<program>.ps1`
/// - Elvish: `$XDG_CONFIG_HOME/elvish/lib/shellcomp/<program>.elv`
///
/// # Errors
///
/// Returns an error if `program_name` is invalid, `HOME`-derived directories cannot be resolved,
/// or the shell is not in the current production support set.
///
/// # Examples
///
/// ```no_run
/// use shellcomp::{Shell, default_install_path};
///
/// let path = default_install_path(Shell::Fish, "demo")?;
/// assert!(path.ends_with("fish/completions/demo.fish"));
/// # Ok::<(), shellcomp::Error>(())
/// ```
pub fn default_install_path(shell: Shell, program_name: &str) -> Result<PathBuf> {
    resolve_default_target_path(&Environment::system(), &shell, program_name)
}

/// Runs a closure with a temporary operation-level hook.
///
/// The hook receives lifecycle events for install, uninstall, detect and migration operations.
/// Events include `Started`, `Succeeded`, and `Failed` phases with `trace_id`, `error_code`, and
/// `retryable` metadata when available.
/// The hook is scoped to the current thread and restored when the closure returns or unwinds.
/// Hooks may temporarily replace or disable themselves with a nested call to this function.
/// An operation started from a hook invokes that hook again unless it has been replaced.
pub fn with_operation_events<R>(
    hook: Option<impl Fn(&OperationEvent) + Send + Sync + 'static>,
    f: impl FnOnce() -> R,
) -> R {
    let hook = hook.map(|hook| {
        let hook: std::sync::Arc<dyn Fn(&OperationEvent) + Send + Sync> = std::sync::Arc::new(hook);
        hook
    });
    with_operation_event_hook(hook, f)
}

/// Installs a completion script and returns a structured report.
///
/// When `path_override` is `None`, the script is written into the shell's managed default
/// location and `shellcomp` attempts to wire activation automatically. When `path_override` is
/// set, legacy behavior is to treat non-default custom paths as manual activation, while an
/// override equal to the managed default path still keeps the default activation semantics.
///
/// This function is idempotent with respect to the written script contents and managed startup
/// wiring. Re-installing an identical script normally returns [`crate::FileChange::Unchanged`].
///
/// # Errors
///
/// Returns [`crate::Error::Failure`] for structured operational failures such as missing `HOME`,
/// unwritable target files, or shell profile update failures.
///
/// Immediate validation problems such as an invalid `program_name` are returned as direct
/// [`crate::Error`] variants instead of [`crate::Error::Failure`].
///
/// # Examples
///
/// ```no_run
/// use shellcomp::{InstallRequest, Shell, install};
///
/// let report = install(InstallRequest {
///     shell: Shell::Zsh,
///     program_name: "demo",
///     script: b"#compdef demo\n",
///     path_override: None,
/// })?;
///
/// assert_eq!(report.shell, Shell::Zsh);
/// # Ok::<(), shellcomp::Error>(())
/// ```
pub fn install(request: InstallRequest<'_>) -> Result<InstallReport> {
    with_operation_trace(|_| install::execute(&Environment::system(), request))
}

/// Installs a completion script with explicit activation intent.
///
/// This is the opt-in API for callers that want a custom path but still want `shellcomp` to
/// manage activation when the shell supports it.
///
/// # Examples
///
/// ```no_run
/// use std::path::PathBuf;
///
/// use shellcomp::{ActivationPolicy, InstallRequest, Shell, install_with_policy};
///
/// let report = install_with_policy(
///     InstallRequest {
///         shell: Shell::Bash,
///         program_name: "demo",
///         script: b"complete -F _demo demo\n",
///         path_override: Some(PathBuf::from("/tmp/demo.bash")),
///     },
///     ActivationPolicy::AutoManaged,
/// )?;
///
/// println!("{report:#?}");
/// # Ok::<(), shellcomp::Error>(())
/// ```
pub fn install_with_policy(
    request: InstallRequest<'_>,
    activation_policy: ActivationPolicy,
) -> Result<InstallReport> {
    with_operation_trace(|_| {
        install::execute_with_policy(&Environment::system(), request, activation_policy)
    })
}

/// Removes a previously managed completion script and any managed activation wiring.
///
/// When `path_override` is set, legacy behavior removes only that file path for non-default custom
/// targets. If the override is equal to the shell's managed default path, uninstall keeps the
/// default cleanup semantics for that shell.
///
/// This function is idempotent. Removing an already absent completion file returns
/// [`crate::FileChange::Absent`] rather than failing.
///
/// # Errors
///
/// Returns [`crate::Error::Failure`] for structured operational failures such as unresolved
/// managed paths or unwritable shell profile files.
///
/// # Examples
///
/// ```no_run
/// use shellcomp::{Shell, UninstallRequest, uninstall};
///
/// let report = uninstall(UninstallRequest {
///     shell: Shell::Bash,
///     program_name: "demo",
///     path_override: None,
/// })?;
///
/// assert_eq!(report.shell, Shell::Bash);
/// # Ok::<(), shellcomp::Error>(())
/// ```
pub fn uninstall(request: UninstallRequest<'_>) -> Result<RemoveReport> {
    with_operation_trace(|_| uninstall::execute(&Environment::system(), request))
}

/// Removes a completion script with explicit activation cleanup intent.
///
/// Use this when the completion file lives at a custom path and you still want `shellcomp` to
/// clean up managed activation wiring for shells such as Bash or Zsh.
pub fn uninstall_with_policy(
    request: UninstallRequest<'_>,
    activation_policy: ActivationPolicy,
) -> Result<RemoveReport> {
    with_operation_trace(|_| {
        uninstall::execute_with_policy(&Environment::system(), request, activation_policy)
    })
}

/// Detects how a completion would be activated for the current environment.
///
/// Detection inspects the default managed location for the given shell and binary name. For custom
/// paths, use [`detect_activation_at_path`].
///
/// The returned [`crate::ActivationReport`] distinguishes the wiring mechanism
/// ([`crate::ActivationMode`]) from current readiness ([`crate::Availability`]).
///
/// # Errors
///
/// Returns [`crate::Error::Failure`] when the managed path or startup wiring cannot be inspected
/// safely.
///
/// # Examples
///
/// ```no_run
/// use shellcomp::{Shell, detect_activation};
///
/// let report = detect_activation(Shell::Fish, "demo")?;
/// println!("{report:#?}");
/// # Ok::<(), shellcomp::Error>(())
/// ```
pub fn detect_activation(shell: Shell, program_name: &str) -> Result<crate::ActivationReport> {
    with_operation_trace(|_| detect::execute(&Environment::system(), shell, program_name))
}

/// Detects activation state for an explicit completion file path.
///
/// This is useful when a caller installed to a custom path and wants detection against that exact
/// file rather than the shell's managed default location. If the explicit path matches the managed
/// default path, detection keeps the shell's default activation semantics.
pub fn detect_activation_at_path(
    shell: Shell,
    program_name: &str,
    target_path: &Path,
) -> Result<crate::ActivationReport> {
    with_operation_trace(|_| {
        detect::execute_at_path(&Environment::system(), shell, program_name, target_path)
    })
}

/// Removes caller-provided legacy managed markers and upserts the equivalent `shellcomp` block.
///
/// This helper is intended for CLI projects that previously shipped their own managed completion
/// blocks and want to migrate to `shellcomp` without leaving duplicate startup wiring behind.
///
/// For shells that do not use a managed startup file, such as Fish, this operation is a no-op.
pub fn migrate_managed_blocks(
    request: MigrateManagedBlocksRequest<'_>,
) -> Result<MigrateManagedBlocksReport> {
    with_operation_trace(|_| migrate::execute(&Environment::system(), request))
}

#[cfg(feature = "clap")]
#[cfg_attr(docsrs, doc(cfg(feature = "clap")))]
fn render_clap_completion_bytes(
    shell: impl Into<Shell>,
    bin_name: &str,
    command: &mut clap::Command,
) -> Result<Vec<u8>> {
    use clap_complete::generate;

    let generator = <clap_complete::Shell as TryFrom<Shell>>::try_from(shell.into())?;
    let mut output = Vec::new();
    generate(generator, command, bin_name, &mut output);
    Ok(output)
}

#[cfg(feature = "clap")]
#[cfg_attr(docsrs, doc(cfg(feature = "clap")))]
/// Renders a completion script from a `clap::CommandFactory` implementation.
///
/// This helper is intentionally optional so the core crate does not require `clap`.
/// It only renders script bytes; installation and activation are still handled by [`install`].
/// The `shell` argument accepts either [`crate::Shell`] or [`crate::clap_complete::Shell`].
/// If you need to tweak or prune the command tree before rendering, use
/// [`render_clap_completion_from_command`] instead.
///
/// # Errors
///
/// Returns [`crate::Error::UnsupportedShell`] for `Shell::Other(_)`.
///
/// # Examples
///
/// ```no_run
/// use clap::Parser;
/// use shellcomp::{InstallRequest, install, render_clap_completion};
///
/// #[derive(Parser)]
/// struct Cli {
///     #[arg(long)]
///     verbose: bool,
/// }
///
/// let generator_shell = shellcomp::clap_complete::Shell::Bash;
/// let script = render_clap_completion::<Cli>(generator_shell, "demo")?;
/// let report = install(InstallRequest {
///     shell: generator_shell.into(),
///     program_name: "demo",
///     script: &script,
///     path_override: None,
/// })?;
///
/// assert!(!script.is_empty());
/// assert_eq!(report.shell, shellcomp::Shell::Bash);
/// # Ok::<(), shellcomp::Error>(())
/// ```
pub fn render_clap_completion<T: clap::CommandFactory>(
    shell: impl Into<Shell>,
    bin_name: &str,
) -> Result<Vec<u8>> {
    let mut command = T::command();
    render_clap_completion_bytes(shell, bin_name, &mut command)
}

#[cfg(feature = "clap")]
#[cfg_attr(docsrs, doc(cfg(feature = "clap")))]
/// Renders a completion script from a prebuilt [`clap::Command`].
///
/// Use this when the caller needs to construct the real command tree first and then apply
/// lightweight adjustments before rendering, such as adding build-specific flags or hiding an
/// internal subcommand from completions.
///
/// This helper is intentionally optional so the core crate does not require `clap`.
/// It only renders script bytes; installation and activation are still handled by [`install`].
/// The `shell` argument accepts either [`crate::Shell`] or [`crate::clap_complete::Shell`].
///
/// # Errors
///
/// Returns [`crate::Error::UnsupportedShell`] for `Shell::Other(_)`.
///
/// # Examples
///
/// ```no_run
/// use clap::{Arg, CommandFactory, Parser};
/// use shellcomp::render_clap_completion_from_command;
///
/// #[derive(Parser)]
/// struct Cli {
///     #[arg(long)]
///     verbose: bool,
/// }
///
/// let command = Cli::command().arg(Arg::new("profile").long("profile"));
/// let script = render_clap_completion_from_command(
///     shellcomp::clap_complete::Shell::Fish,
///     "demo",
///     command,
/// )?;
///
/// assert!(!script.is_empty());
/// # Ok::<(), shellcomp::Error>(())
/// ```
pub fn render_clap_completion_from_command(
    shell: impl Into<Shell>,
    bin_name: &str,
    mut command: clap::Command,
) -> Result<Vec<u8>> {
    render_clap_completion_bytes(shell, bin_name, &mut command)
}

#[cfg(all(test, feature = "clap"))]
mod clap_tests {
    use clap::{Arg, CommandFactory, Parser};

    use super::{render_clap_completion, render_clap_completion_from_command};
    use crate::Shell;

    #[derive(Parser)]
    struct TestCli {
        #[arg(long)]
        verbose: bool,
    }

    #[test]
    fn renders_clap_completion() {
        let script = render_clap_completion::<TestCli>(Shell::Bash, "test-cli")
            .expect("bash completion should render");
        let rendered = String::from_utf8(script).expect("completion output should be utf-8");
        assert!(rendered.contains("test-cli"));
        assert!(rendered.contains("complete -F _test__cli "));
        assert!(rendered.contains("--verbose"));
    }

    #[test]
    fn renders_clap_completion_from_clap_complete_shell() {
        let script =
            render_clap_completion::<TestCli>(crate::clap_complete::Shell::Fish, "test-cli")
                .expect("fish completion should render");
        let rendered = String::from_utf8(script).expect("completion output should be utf-8");
        assert!(rendered.contains("test-cli"));
    }

    #[test]
    fn renders_clap_completion_from_adjusted_command() {
        let command = TestCli::command().arg(Arg::new("profile").long("profile"));
        let script = render_clap_completion_from_command(Shell::Bash, "test-cli", command)
            .expect("bash completion should render from an adjusted command");
        let rendered = String::from_utf8(script).expect("completion output should be utf-8");

        assert!(rendered.contains("test-cli"));
        assert!(rendered.contains("--profile"));
    }

    #[test]
    fn rejects_other_shell_for_clap_generation() {
        let error = render_clap_completion::<TestCli>(Shell::Other("xonsh".to_owned()), "test-cli")
            .expect_err("unsupported shell should fail");

        assert!(matches!(
            error,
            crate::Error::UnsupportedShell(Shell::Other(value)) if value == "xonsh"
        ));
    }

    #[test]
    fn rejects_other_shell_for_prebuilt_clap_command() {
        let command = TestCli::command();
        let error = render_clap_completion_from_command(
            Shell::Other("xonsh".to_owned()),
            "test-cli",
            command,
        )
        .expect_err("unsupported shell should fail");

        assert!(matches!(
            error,
            crate::Error::UnsupportedShell(Shell::Other(value)) if value == "xonsh"
        ));
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{InstallRequest, install};

    #[test]
    fn api_surface_attaches_trace_id_to_structural_failure() {
        let error = install(InstallRequest {
            shell: crate::Shell::Bash,
            program_name: "tool",
            script: b"complete -F _tool tool\n",
            path_override: Some(PathBuf::from("tool.bash")),
        })
        .expect_err("install should fail with validation error");

        let report = crate::tests::assert_structural_failure(error, "api-install");
        assert_ne!(report.trace_id, 0);
    }
}
