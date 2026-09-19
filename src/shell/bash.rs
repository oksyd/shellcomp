use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::infra::{
    env::Environment,
    fs,
    managed_block::{self, ManagedBlock},
    paths,
};
use crate::model::LegacyManagedBlock;
use crate::model::{ActivationMode, ActivationReport, Availability, CleanupReport, Shell};
use crate::shell::{ActivationOutcome, CleanupOutcome, MigrationOutcome, migrate_profile_blocks};

const BASH_LOADER_PATHS: &[&str] = &[
    "/usr/share/bash-completion/bash_completion",
    "/etc/bash_completion",
    "/usr/local/share/bash-completion/bash_completion",
    "/usr/local/etc/profile.d/bash_completion.sh",
    "/opt/homebrew/etc/profile.d/bash_completion.sh",
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum LoaderStatus {
    ActiveNow,
    WiredInStartup(PathBuf),
    PresentButUnwired,
    Absent,
}

pub(crate) fn install(
    env: &Environment,
    program_name: &str,
    target_path: &Path,
) -> Result<ActivationOutcome> {
    let rc_path = bashrc_path(env)?;
    let can_use_system_loader = target_uses_system_loader_path(env, program_name, target_path);

    let loader_status = if can_use_system_loader {
        Some(loader_status(env)?)
    } else {
        None
    };

    if let Some(loader_status) = &loader_status {
        match loader_status {
            LoaderStatus::ActiveNow => {
                return Ok(ActivationOutcome {
                    report: ActivationReport {
                        mode: ActivationMode::SystemLoader,
                        availability: Availability::ActiveNow,
                        location: Some(target_path.to_path_buf()),
                        reason: Some(
                            "Detected an active bash-completion loader in the current shell."
                                .to_owned(),
                        ),
                        next_step: None,
                    },
                    affected_locations: Vec::new(),
                });
            }
            LoaderStatus::WiredInStartup(startup_path) => {
                return Ok(ActivationOutcome {
                    report: ActivationReport {
                        mode: ActivationMode::SystemLoader,
                        availability: Availability::AvailableAfterNewShell,
                        location: Some(startup_path.clone()),
                        reason: Some(format!(
                            "Detected startup-file wiring for a system bash-completion loader in `{}`.",
                            startup_path.display()
                        )),
                        next_step: Some(
                            "Start a new Bash session to ensure the completion script is loaded."
                                .to_owned(),
                        ),
                    },
                    affected_locations: vec![startup_path.clone()],
                });
            }
            LoaderStatus::PresentButUnwired | LoaderStatus::Absent => {}
        }
    }

    let block = managed_block(program_name, target_path)?;
    managed_block::upsert(&rc_path, &block)?;

    Ok(ActivationOutcome {
        report: ActivationReport {
            mode: ActivationMode::ManagedRcBlock,
            availability: Availability::AvailableAfterSource,
            location: Some(rc_path.clone()),
            reason: Some(match loader_status {
                Some(LoaderStatus::PresentButUnwired) => {
                    "A known bash-completion loader file exists on disk, but ~/.bashrc does not appear to source it, so shellcomp added a managed block to ~/.bashrc.".to_owned()
                }
                Some(LoaderStatus::Absent | LoaderStatus::ActiveNow | LoaderStatus::WiredInStartup(_)) => {
                    "No system bash-completion loader was detected, so shellcomp added a managed block to ~/.bashrc."
                        .to_owned()
                }
                None => {
                    "Installed to a custom Bash completion path, so shellcomp added a managed block to ~/.bashrc to source it directly."
                        .to_owned()
                }
            }),
            next_step: Some(format!(
                "Run `source {}` or start a new Bash session.",
                shell_quote(&rc_path)?
            )),
        },
        affected_locations: vec![rc_path],
    })
}

pub(crate) fn uninstall(
    env: &Environment,
    program_name: &str,
    target_path: &Path,
) -> Result<CleanupOutcome> {
    let rc_path = bashrc_path(env)?;
    let block = managed_block(program_name, target_path)?;
    let can_use_system_loader = target_uses_system_loader_path(env, program_name, target_path);
    let loader_status = if can_use_system_loader {
        Some(loader_status(env)?)
    } else {
        None
    };
    let rc_change = managed_block::remove(&rc_path, &block)?;

    let (mode, location, reason, mut affected_locations) = match (&rc_change, loader_status) {
        (crate::FileChange::Absent, Some(LoaderStatus::ActiveNow)) => (
            ActivationMode::SystemLoader,
            None,
            "No shellcomp-managed Bash activation block was present; Bash completion relied on an active system loader."
                .to_owned(),
            vec![rc_path.clone()],
        ),
        (crate::FileChange::Absent, Some(LoaderStatus::WiredInStartup(startup_path))) => (
            ActivationMode::SystemLoader,
            Some(startup_path.clone()),
            format!(
                "No shellcomp-managed Bash activation block was present; Bash completion was wired through the system loader in `{}`.",
                startup_path.display()
            ),
            vec![rc_path.clone(), startup_path],
        ),
        _ => (
            ActivationMode::ManagedRcBlock,
            Some(rc_path.clone()),
            match rc_change {
                crate::FileChange::Removed => {
                    "Removed the managed Bash activation block from ~/.bashrc.".to_owned()
                }
                crate::FileChange::Absent => {
                    "No managed Bash activation block was present in ~/.bashrc.".to_owned()
                }
                _ => "Bash activation cleanup completed.".to_owned(),
            },
            vec![rc_path.clone()],
        ),
    };

    Ok(CleanupOutcome {
        cleanup: CleanupReport {
            mode,
            change: rc_change,
            location,
            reason: Some(reason),
            next_step: None,
        },
        affected_locations: {
            affected_locations.shrink_to_fit();
            affected_locations
        },
    })
}

pub(crate) fn detect(
    env: &Environment,
    program_name: &str,
    target_path: &Path,
) -> Result<ActivationReport> {
    let rc_path = bashrc_path(env)?;
    let can_use_system_loader = target_uses_system_loader_path(env, program_name, target_path);
    let loader_status = if can_use_system_loader {
        Some(loader_status(env)?)
    } else {
        None
    };
    let block = managed_block(program_name, target_path)?;
    let wired = managed_block::matches(&rc_path, &block)?;

    if !fs::file_exists(target_path)? {
        let mode = if matches!(
            loader_status,
            Some(LoaderStatus::ActiveNow | LoaderStatus::WiredInStartup(_))
        ) {
            ActivationMode::SystemLoader
        } else {
            ActivationMode::ManagedRcBlock
        };
        return Ok(ActivationReport {
            mode,
            availability: Availability::ManualActionRequired,
            location: Some(if matches!(mode, ActivationMode::ManagedRcBlock) && wired {
                rc_path.clone()
            } else {
                target_path.to_path_buf()
            }),
            reason: Some(if matches!(mode, ActivationMode::ManagedRcBlock) && wired {
                format!(
                    "Managed Bash activation block is present in ~/.bashrc, but completion file `{}` is not installed.",
                    target_path.display()
                )
            } else {
                format!(
                    "Completion file `{}` is not installed.",
                    target_path.display()
                )
            }),
            next_step: Some(
                "Run your CLI's completion install command or install the script manually."
                    .to_owned(),
            ),
        });
    }

    if let Some(loader_status) = loader_status {
        match loader_status {
            LoaderStatus::ActiveNow => {
                return Ok(ActivationReport {
                    mode: ActivationMode::SystemLoader,
                    availability: Availability::ActiveNow,
                    location: Some(target_path.to_path_buf()),
                    reason: Some(
                        "Detected an active bash-completion loader in the current shell."
                            .to_owned(),
                    ),
                    next_step: None,
                });
            }
            LoaderStatus::WiredInStartup(startup_path) => {
                return Ok(ActivationReport {
                    mode: ActivationMode::SystemLoader,
                    availability: Availability::AvailableAfterNewShell,
                    location: Some(startup_path.clone()),
                    reason: Some(format!(
                        "Detected startup-file wiring for a system bash-completion loader in `{}`.",
                        startup_path.display()
                    )),
                    next_step: Some(
                        "Start a new Bash session if completions are not available yet.".to_owned(),
                    ),
                });
            }
            LoaderStatus::PresentButUnwired | LoaderStatus::Absent => {}
        }
    }

    let quoted_rc_path = shell_quote(&rc_path)?;
    let quoted_target_path = shell_quote(target_path)?;

    Ok(ActivationReport {
        mode: ActivationMode::ManagedRcBlock,
        availability: if wired {
            Availability::AvailableAfterSource
        } else {
            Availability::ManualActionRequired
        },
        location: Some(rc_path),
        reason: Some(if wired {
            "Managed Bash activation block is present in ~/.bashrc.".to_owned()
        } else {
            "Completion file exists, but the managed Bash activation block was not found."
                .to_owned()
        }),
        next_step: Some(if wired {
            format!("Run `source {quoted_rc_path}` or start a new Bash session.")
        } else {
            format!(
                "Re-run installation or source {quoted_target_path} from {quoted_rc_path} manually."
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
    let rc_path = bashrc_path(env)?;
    let block = managed_block(program_name, target_path)?;
    let (legacy_change, managed_change) = migrate_profile_blocks(&rc_path, legacy_blocks, &block)?;

    Ok(MigrationOutcome {
        location: Some(rc_path.clone()),
        managed_change,
        legacy_change,
        affected_locations: vec![rc_path],
    })
}

fn managed_block(program_name: &str, target_path: &Path) -> Result<ManagedBlock> {
    let quoted = shell_quote(target_path)?;
    Ok(ManagedBlock {
        start_marker: format!("# >>> shellcomp bash {program_name} >>>"),
        end_marker: format!("# <<< shellcomp bash {program_name} <<<"),
        body: format!("if [ -f {quoted} ]; then\n  . {quoted}\nfi"),
    })
}

fn shell_quote(path: &Path) -> Result<String> {
    let value = path.to_str().ok_or_else(|| Error::NonUtf8Path {
        path: path.to_path_buf(),
    })?;
    Ok(format!("'{}'", value.replace('\'', "'\"'\"'")))
}

fn bashrc_path(env: &Environment) -> Result<PathBuf> {
    Ok(env.home_dir()?.join(".bashrc"))
}

fn target_uses_system_loader_path(
    env: &Environment,
    program_name: &str,
    target_path: &Path,
) -> bool {
    paths::default_install_path(env, &Shell::Bash, program_name)
        .map(|default_path| default_path == target_path)
        .unwrap_or(false)
}

fn loader_status(env: &Environment) -> Result<LoaderStatus> {
    if loader_active_now(env) {
        return Ok(LoaderStatus::ActiveNow);
    }

    if let Some(startup_path) = startup_file_wiring(env)? {
        return Ok(LoaderStatus::WiredInStartup(startup_path));
    }

    if loader_file_present(env) {
        return Ok(LoaderStatus::PresentButUnwired);
    }

    Ok(LoaderStatus::Absent)
}

fn loader_file_present(env: &Environment) -> bool {
    BASH_LOADER_PATHS
        .iter()
        .any(|path| env.path_exists(Path::new(path)))
}

fn startup_file_wiring(env: &Environment) -> Result<Option<PathBuf>> {
    let mut visited = BTreeSet::new();
    for startup_path in startup_files(env)? {
        if let Some(wired_path) = file_reaches_loader(env, &startup_path, &mut visited)? {
            return Ok(Some(wired_path));
        }
    }

    Ok(None)
}

fn startup_files(env: &Environment) -> Result<Vec<PathBuf>> {
    let mut files = vec![PathBuf::from("/etc/bash.bashrc")];
    if let Ok(home) = env.home_dir() {
        push_unique_path(&mut files, home.join(".bashrc"));
    }

    push_unique_path(&mut files, PathBuf::from("/etc/profile"));
    if let Ok(home) = env.home_dir()
        && let Some(login_file) = first_existing_login_startup_file(env, &home)?
    {
        push_unique_path(&mut files, login_file);
    }

    Ok(files)
}

fn first_existing_login_startup_file(env: &Environment, home: &Path) -> Result<Option<PathBuf>> {
    for candidate in [
        home.join(".bash_profile"),
        home.join(".bash_login"),
        home.join(".profile"),
    ] {
        if env.read_file_if_exists(&candidate)?.is_some() {
            return Ok(Some(candidate));
        }
    }

    Ok(None)
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

/// Exclude quoted continuations and here-document bodies before inspecting commands.
/// The line lexer alone cannot distinguish these from executable startup code.
fn command_lines(contents: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut quote = None;
    let mut continued = false;
    let mut documents = std::collections::VecDeque::new();
    for line in contents.split_inclusive('\n') {
        if let Some((delimiter, strip_tabs)) = documents.front() {
            let body = line.strip_suffix('\n').unwrap_or(line);
            let body = if *strip_tabs {
                body.trim_start_matches('\t')
            } else {
                body
            };
            if body == delimiter {
                documents.pop_front();
            }
            continue;
        }
        let mut eligible = quote.is_none() && !continued;
        continued = false;
        let mut escaped = false;
        let mut boundary = true;
        let mut chars = line.char_indices().peekable();
        while let Some((index, ch)) = chars.next() {
            if escaped {
                escaped = false;
                if ch == '\n' {
                    continued = true;
                }
                continue;
            }
            if quote != Some('\'') && ch == '\\' {
                escaped = true;
                boundary = false;
                continue;
            }
            if let Some(delimiter) = quote {
                if ch == delimiter {
                    quote = None;
                }
                continue;
            }
            match ch {
                '#' if boundary => break,
                '\'' | '"' => {
                    quote = Some(ch);
                    boundary = false;
                }
                '<' if chars.peek().is_some_and(|(_, next)| *next == '<') => {
                    chars.next();
                    eligible = false;
                    if chars.peek().is_some_and(|(_, next)| *next == '<') {
                        chars.next(); // A here-string has no following body.
                        continue;
                    }
                    let tail = &line[index + 2..];
                    let strip_tabs = tail.starts_with('-');
                    let tail = if strip_tabs { &tail[1..] } else { tail };
                    let tail = tail.trim_start_matches([' ', '\t']);
                    let mut end = 0;
                    let mut delimiter_quote = None;
                    for (offset, ch) in tail.char_indices() {
                        if let Some(delimiter) = delimiter_quote {
                            if ch == delimiter {
                                delimiter_quote = None;
                            }
                        } else if matches!(ch, '\'' | '"') {
                            delimiter_quote = Some(ch);
                        } else if ch.is_whitespace() || matches!(ch, ';' | '&' | '|' | '<' | '>') {
                            break;
                        }
                        end = offset + ch.len_utf8();
                    }
                    let delimiter = unquote_shell_token(&tail[..end]);
                    if delimiter.is_empty()
                        || delimiter_quote.is_some()
                        || delimiter.contains(['\'', '"', '\\'])
                    {
                        // Unsupported delimiter syntax: do not inspect a possible data body.
                        return Vec::new();
                    }
                    documents.push_back((delimiter.to_owned(), strip_tabs));
                    boundary = true;
                }
                _ => boundary = ch.is_whitespace() || matches!(ch, ';' | '&' | '|' | '<' | '>'),
            }
        }
        if eligible && quote.is_none() && !continued && !escaped {
            result.push(line.trim_end_matches('\n'));
        }
    }
    result
}

fn file_reaches_loader(
    env: &Environment,
    startup_path: &Path,
    visited: &mut BTreeSet<PathBuf>,
) -> Result<Option<PathBuf>> {
    enum Probe {
        File(PathBuf),
        Directory(&'static Path),
    }

    let mut pending = vec![Probe::File(startup_path.to_path_buf())];
    while let Some(probe) = pending.pop() {
        let startup_path = match probe {
            Probe::File(path) => path,
            Probe::Directory(directory) => {
                let mut entries = env.read_dir_entries(directory)?;
                entries.sort();
                pending.extend(
                    entries
                        .into_iter()
                        .rev()
                        .filter(|entry| {
                            entry.extension().and_then(|extension| extension.to_str()) == Some("sh")
                        })
                        .map(Probe::File),
                );
                continue;
            }
        };
        if !visited.insert(startup_path.clone()) {
            continue;
        }
        let Some(contents) = read_utf8_file_if_exists(env, &startup_path)? else {
            continue;
        };
        let contents = command_lines(&contents).join("\n");
        if BASH_LOADER_PATHS
            .iter()
            .filter(|path| env.path_exists(Path::new(path)))
            .any(|path| contents.lines().any(|line| line_sources_loader(line, path)))
        {
            return Ok(Some(startup_path));
        }

        // Keep depth-first ordering without putting user-controlled source chains on the stack.
        let mut children = Vec::new();
        for directory in [
            Path::new("/etc/profile.d"),
            Path::new("/usr/local/etc/profile.d"),
            Path::new("/opt/homebrew/etc/profile.d"),
        ] {
            if sources_profile_directory(&contents, directory) {
                children.push(Probe::Directory(directory));
            }
        }
        children.extend(
            contents
                .lines()
                .flat_map(line_source_targets)
                .filter_map(|target| resolve_sourced_path(env, target))
                .map(Probe::File),
        );
        pending.extend(children.into_iter().rev());
    }
    Ok(None)
}

fn resolve_sourced_path(env: &Environment, target: &str) -> Option<PathBuf> {
    let single_quoted = target.starts_with('\'');
    let quoted = single_quoted || target.starts_with('"');
    let target = unquote_shell_token(target);
    // Mixed quoting and escapes require shell evaluation; do not guess their expansion.
    if target.contains(['\\', '\'', '"', '`']) {
        return None;
    }
    let literal = |value: &str| {
        (single_quoted || !value.contains('$'))
            && (quoted || !value.contains(['*', '?', '[', ']', '{', '}']))
    };
    if Path::new(target).is_absolute() {
        return literal(target).then(|| PathBuf::from(target));
    }

    if single_quoted {
        return None;
    }
    let home = env.home_dir().ok()?;
    if !quoted && let Some(path) = target.strip_prefix("~/") {
        return literal(path).then(|| home.join(path));
    }
    if let Some(path) = target
        .strip_prefix("$HOME/")
        .or_else(|| target.strip_prefix("${HOME}/"))
    {
        // An unquoted parameter expansion can split into words or expand globs.
        if !quoted
            && home.to_str().is_none_or(|value| {
                value.chars().any(char::is_whitespace) || value.contains(['*', '?', '[', ']'])
            })
        {
            return None;
        }
        return literal(path).then(|| home.join(path));
    }

    None
}

fn line_sources_loader(line: &str, loader_path: &str) -> bool {
    line_source_targets(line)
        .into_iter()
        .any(|target| unquote_shell_token(target) == loader_path)
}

fn sources_profile_directory(contents: &str, profile_dir: &Path) -> bool {
    let glob = format!("{}/*.sh", profile_dir.display());
    contents.lines().flat_map(simple_commands).any(|words| {
        let ["for", variable, "in", pattern] = words.as_slice() else { return false; };
        if *pattern != glob { return false; }
        let plain = format!("${variable}");
        let braced = format!("${{{variable}}}");
        contents.lines().flat_map(line_source_targets).any(|target| {
            !target.starts_with('\'') && matches!(unquote_shell_token(target), value if value == plain || value == braced)
        })
    })
}

/// Recognize simple commands without treating quoted text or comments as shell code.
/// Complex execution contexts are deliberately left unclassified.
fn simple_commands(line: &str) -> Vec<Vec<&str>> {
    let mut commands = Vec::new();
    let mut words = Vec::new();
    let mut start = None;
    let mut quote = None;
    let mut escaped = false;
    let mut chars = line.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if escaped {
            escaped = false;
            continue;
        }
        if quote != Some('\'') && ch == '\\' {
            start.get_or_insert(index);
            escaped = true;
            continue;
        }
        if quote != Some('\'')
            && (ch == '`' || (ch == '$' && chars.peek().is_some_and(|(_, ch)| *ch == '(')))
        {
            return Vec::new();
        }
        if let Some(delimiter) = quote {
            if ch == delimiter {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => {
                start.get_or_insert(index);
                quote = Some(ch);
            }
            '#' if start.is_none() => break,
            ';' | '&' | '|' => {
                if ch != ';' {
                    if chars.peek().is_none_or(|(_, next)| *next != ch) {
                        return Vec::new(); // Pipelines and background jobs do not wire this shell.
                    }
                    chars.next();
                }
                if let Some(begin) = start.take() {
                    words.push(&line[begin..index]);
                }
                if !words.is_empty() {
                    commands.push(std::mem::take(&mut words));
                }
            }
            '(' | ')' | '<' | '>' => return Vec::new(),
            ch if ch.is_whitespace() => {
                if let Some(begin) = start.take() {
                    words.push(&line[begin..index]);
                }
            }
            _ => {
                start.get_or_insert(index);
            }
        }
    }
    if quote.is_some() || escaped {
        return Vec::new();
    }
    if let Some(begin) = start {
        words.push(&line[begin..]);
    }
    if !words.is_empty() {
        commands.push(words);
    }
    commands
}

fn line_source_targets(line: &str) -> Vec<&str> {
    simple_commands(line)
        .into_iter()
        .filter_map(|words| {
            let words = match words.first().copied() {
                Some("then" | "do") => &words[1..],
                _ => &words[..],
            };
            match words {
                ["source" | ".", target, ..] => Some(*target),
                _ => None,
            }
        })
        .collect()
}

fn unquote_shell_token(token: &str) -> &str {
    token
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
        .or_else(|| {
            token
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
        })
        .unwrap_or(token)
}

fn read_utf8_file_if_exists(env: &Environment, path: &Path) -> Result<Option<String>> {
    match env.read_file_if_exists(path)? {
        Some(contents) => {
            String::from_utf8(contents)
                .map(Some)
                .map_err(|_| Error::InvalidUtf8File {
                    path: path.to_path_buf(),
                })
        }
        None => Ok(None),
    }
}

fn loader_active_now(env: &Environment) -> bool {
    env.var_os("BASH_COMPLETION_VERSINFO").is_some()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::{LoaderStatus, detect, install, loader_active_now, loader_status};
    use crate::infra::env::Environment;
    use crate::model::{ActivationMode, Availability};

    #[test]
    fn loader_status_is_active_when_env_hint_is_present() {
        let env = Environment::test().with_var("BASH_COMPLETION_VERSINFO", "2");
        let status = loader_status(&env).expect("status should resolve");

        assert_eq!(status, LoaderStatus::ActiveNow);
        assert!(loader_active_now(&env));
    }

    #[test]
    fn loader_status_is_present_but_unwired_when_only_loader_file_exists() {
        let temp_root = crate::tests::temp_dir("bash-loader-present-but-unwired");
        let home = temp_root.join("home");
        fs::create_dir_all(&home).expect("home should be creatable");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("BASH_COMPLETION_VERSINFO")
            .with_existing_path("/usr/share/bash-completion/bash_completion")
            .without_real_path_lookups();
        let status = loader_status(&env).expect("status should resolve");

        assert_eq!(status, LoaderStatus::PresentButUnwired);
    }

    #[test]
    fn loader_status_is_wired_when_bash_profile_sources_known_loader() {
        let temp_root = crate::tests::temp_dir("bash-loader-bash-profile");
        let home = temp_root.join("home");
        fs::create_dir_all(&home).expect("home should be creatable");
        fs::write(
            home.join(".bash_profile"),
            "source /usr/share/bash-completion/bash_completion\n",
        )
        .expect(".bash_profile should be writable");

        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("BASH_COMPLETION_VERSINFO")
            .with_existing_path("/usr/share/bash-completion/bash_completion")
            .without_real_path_lookups();
        let status = loader_status(&env).expect("status should resolve");

        assert_eq!(
            status,
            LoaderStatus::WiredInStartup(home.join(".bash_profile"))
        );
    }

    #[test]
    fn loader_status_accepts_tab_after_source_keyword() {
        let temp_root = crate::tests::temp_dir("bash-loader-source-tab");
        let home = temp_root.join("home");
        fs::create_dir_all(&home).expect("home should be creatable");
        fs::write(
            home.join(".bash_profile"),
            "source\t/usr/share/bash-completion/bash_completion\n",
        )
        .expect(".bash_profile should be writable");

        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("BASH_COMPLETION_VERSINFO")
            .with_existing_path("/usr/share/bash-completion/bash_completion")
            .without_real_path_lookups();
        let status = loader_status(&env).expect("status should resolve");

        assert_eq!(
            status,
            LoaderStatus::WiredInStartup(home.join(".bash_profile"))
        );
    }

    #[test]
    fn loader_status_accepts_tab_separated_then_dot_bashrc_chain() {
        let temp_root = crate::tests::temp_dir("bash-loader-then-dot-tab-chain");
        let home = temp_root.join("home");
        fs::create_dir_all(&home).expect("home should be creatable");
        fs::write(
            home.join(".bash_profile"),
            "if [ -f \"$HOME/.bashrc\" ]; then\t.\t\"$HOME/.bashrc\"; fi\n",
        )
        .expect(".bash_profile should be writable");
        fs::write(
            home.join(".bashrc"),
            "source /usr/share/bash-completion/bash_completion\n",
        )
        .expect(".bashrc should be writable");

        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("BASH_COMPLETION_VERSINFO")
            .with_existing_path("/usr/share/bash-completion/bash_completion")
            .without_real_path_lookups();

        let status = loader_status(&env).expect("status should resolve");

        assert_eq!(status, LoaderStatus::WiredInStartup(home.join(".bashrc")));
    }

    #[test]
    fn loader_status_follows_tilde_sourced_bashrc_chain() {
        let temp_root = crate::tests::temp_dir("bash-loader-tilde-chain");
        let home = temp_root.join("home");
        fs::create_dir_all(&home).expect("home should be creatable");
        fs::write(
            home.join(".bash_profile"),
            "if [ -f ~/.bashrc ]; then\n  . ~/.bashrc\nfi\n",
        )
        .expect(".bash_profile should be writable");
        fs::write(
            home.join(".bashrc"),
            "source /usr/share/bash-completion/bash_completion\n",
        )
        .expect(".bashrc should be writable");

        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("BASH_COMPLETION_VERSINFO")
            .with_existing_path("/usr/share/bash-completion/bash_completion")
            .without_real_path_lookups();

        let status = loader_status(&env).expect("status should resolve");

        assert_eq!(status, LoaderStatus::WiredInStartup(home.join(".bashrc")));
    }

    #[test]
    fn loader_status_follows_home_expanded_bashrc_chain() {
        let temp_root = crate::tests::temp_dir("bash-loader-home-chain");
        let home = temp_root.join("home");
        fs::create_dir_all(&home).expect("home should be creatable");
        fs::write(
            home.join(".bash_profile"),
            "if [ -f \"$HOME/.bashrc\" ]; then\n  source \"$HOME/.bashrc\"\nfi\n",
        )
        .expect(".bash_profile should be writable");
        fs::write(
            home.join(".bashrc"),
            "source /usr/share/bash-completion/bash_completion\n",
        )
        .expect(".bashrc should be writable");

        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("BASH_COMPLETION_VERSINFO")
            .with_existing_path("/usr/share/bash-completion/bash_completion")
            .without_real_path_lookups();

        let status = loader_status(&env).expect("status should resolve");

        assert_eq!(status, LoaderStatus::WiredInStartup(home.join(".bashrc")));
    }

    #[test]
    fn loader_status_is_wired_when_bashrc_sources_etc_bashrc_that_sources_loader() {
        let temp_root = crate::tests::temp_dir("bash-loader-via-etc-bashrc");
        let home = temp_root.join("home");
        fs::create_dir_all(&home).expect("home should be creatable");
        fs::write(home.join(".bashrc"), "source /etc/bashrc\n")
            .expect(".bashrc should be writable");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("BASH_COMPLETION_VERSINFO")
            .with_existing_path("/usr/share/bash-completion/bash_completion")
            .with_file_contents(
                "/etc/bashrc",
                "source /usr/share/bash-completion/bash_completion\n",
            )
            .without_real_path_lookups();

        let status = loader_status(&env).expect("status should resolve");

        assert_eq!(
            status,
            LoaderStatus::WiredInStartup(PathBuf::from("/etc/bashrc"))
        );
    }

    #[test]
    fn loader_status_does_not_assume_etc_bashrc_is_reachable_without_startup_wiring() {
        let env = Environment::test()
            .with_var("HOME", "/tmp/test-home")
            .without_var("BASH_COMPLETION_VERSINFO")
            .with_existing_path("/usr/share/bash-completion/bash_completion")
            .with_file_contents(
                "/etc/bashrc",
                "source /usr/share/bash-completion/bash_completion\n",
            )
            .without_real_path_lookups();

        let status = loader_status(&env).expect("status should resolve");

        assert_eq!(status, LoaderStatus::PresentButUnwired);
    }

    #[test]
    fn loader_status_is_wired_when_profile_d_script_sources_known_loader() {
        let env = Environment::test()
            .with_var("HOME", "/tmp/test-home")
            .without_var("BASH_COMPLETION_VERSINFO")
            .with_existing_path("/usr/share/bash-completion/bash_completion")
            .with_file_contents(
                "/etc/profile",
                "for i in /etc/profile.d/*.sh; do\n  [ -r \"$i\" ] && . \"$i\"\ndone\n",
            )
            .with_dir_entries(
                "/etc/profile.d",
                [PathBuf::from("/etc/profile.d/bash-completion.sh")],
            )
            .with_file_contents(
                "/etc/profile.d/bash-completion.sh",
                "source /usr/share/bash-completion/bash_completion\n",
            )
            .without_real_path_lookups();

        let status = loader_status(&env).expect("status should resolve");

        assert_eq!(
            status,
            LoaderStatus::WiredInStartup(PathBuf::from("/etc/profile.d/bash-completion.sh"))
        );
    }

    #[test]
    fn loader_status_does_not_assume_profile_d_is_reachable_without_startup_wiring() {
        let env = Environment::test()
            .with_var("HOME", "/tmp/test-home")
            .without_var("BASH_COMPLETION_VERSINFO")
            .with_existing_path("/usr/share/bash-completion/bash_completion")
            .with_dir_entries(
                "/etc/profile.d",
                [PathBuf::from("/etc/profile.d/bash-completion.sh")],
            )
            .with_file_contents(
                "/etc/profile.d/bash-completion.sh",
                "source /usr/share/bash-completion/bash_completion\n",
            )
            .without_real_path_lookups();

        let status = loader_status(&env).expect("status should resolve");

        assert_eq!(status, LoaderStatus::PresentButUnwired);
    }

    #[test]
    fn loader_status_does_not_treat_unrelated_profile_d_source_as_loader_wiring() {
        let env = Environment::test()
            .with_var("HOME", "/tmp/test-home")
            .without_var("BASH_COMPLETION_VERSINFO")
            .with_existing_path("/usr/share/bash-completion/bash_completion")
            .with_file_contents("/etc/profile", "source /etc/profile.d/other.sh\n")
            .with_dir_entries(
                "/etc/profile.d",
                [
                    PathBuf::from("/etc/profile.d/bash-completion.sh"),
                    PathBuf::from("/etc/profile.d/other.sh"),
                ],
            )
            .with_file_contents(
                "/etc/profile.d/bash-completion.sh",
                "source /usr/share/bash-completion/bash_completion\n",
            )
            .with_file_contents("/etc/profile.d/other.sh", "echo unrelated\n")
            .without_real_path_lookups();

        let status = loader_status(&env).expect("status should resolve");

        assert_eq!(status, LoaderStatus::PresentButUnwired);
    }

    #[test]
    fn loader_status_is_wired_when_profile_directly_sources_non_sh_profile_d_script() {
        let env = Environment::test()
            .with_var("HOME", "/tmp/test-home")
            .without_var("BASH_COMPLETION_VERSINFO")
            .with_existing_path("/usr/share/bash-completion/bash_completion")
            .with_file_contents("/etc/profile", "source /etc/profile.d/bash_completion\n")
            .with_file_contents(
                "/etc/profile.d/bash_completion",
                "source /usr/share/bash-completion/bash_completion\n",
            )
            .without_real_path_lookups();

        let status = loader_status(&env).expect("status should resolve");

        assert_eq!(
            status,
            LoaderStatus::WiredInStartup(PathBuf::from("/etc/profile.d/bash_completion"))
        );
    }

    #[test]
    fn loader_status_does_not_treat_run_parts_child_processes_as_wiring() {
        let env = Environment::test()
            .with_var("HOME", "/tmp/test-home")
            .without_var("BASH_COMPLETION_VERSINFO")
            .with_existing_path("/usr/share/bash-completion/bash_completion")
            .with_file_contents("/etc/profile", "run-parts /etc/profile.d\n")
            .with_dir_entries(
                "/etc/profile.d",
                [PathBuf::from("/etc/profile.d/bash_completion")],
            )
            .with_file_contents(
                "/etc/profile.d/bash_completion",
                "source /usr/share/bash-completion/bash_completion\n",
            )
            .without_real_path_lookups();

        let status = loader_status(&env).expect("status should resolve");

        assert_eq!(status, LoaderStatus::PresentButUnwired);
    }

    #[test]
    fn loader_status_does_not_treat_different_profile_d_prefix_as_reachable() {
        let env = Environment::test()
            .with_var("HOME", "/tmp/test-home")
            .without_var("BASH_COMPLETION_VERSINFO")
            .with_existing_path("/usr/share/bash-completion/bash_completion")
            .with_file_contents("/etc/profile", "for i in /etc/profile.d-custom/*.sh; do . \"$i\"; done\nrun-parts /etc/profile.d-custom\n")
            .with_dir_entries(
                "/etc/profile.d",
                [PathBuf::from("/etc/profile.d/bash-completion.sh")],
            )
            .with_file_contents(
                "/etc/profile.d/bash-completion.sh",
                "source /usr/share/bash-completion/bash_completion\n",
            )
            .without_real_path_lookups();

        let status = loader_status(&env).expect("status should resolve");

        assert_eq!(status, LoaderStatus::PresentButUnwired);
    }

    #[test]
    fn loader_status_does_not_treat_echoed_profile_d_glob_as_wiring() {
        let env = Environment::test()
            .with_var("HOME", "/tmp/test-home")
            .without_var("BASH_COMPLETION_VERSINFO")
            .with_existing_path("/usr/share/bash-completion/bash_completion")
            .with_file_contents("/etc/profile", "echo /etc/profile.d/*.sh >/dev/null\n")
            .with_dir_entries(
                "/etc/profile.d",
                [PathBuf::from("/etc/profile.d/bash-completion.sh")],
            )
            .with_file_contents(
                "/etc/profile.d/bash-completion.sh",
                "source /usr/share/bash-completion/bash_completion\n",
            )
            .without_real_path_lookups();

        let status = loader_status(&env).expect("status should resolve");

        assert_eq!(status, LoaderStatus::PresentButUnwired);
    }

    #[test]
    fn loader_status_respects_login_file_precedence() {
        let temp_root = crate::tests::temp_dir("bash-loader-login-precedence");
        let home = temp_root.join("home");
        fs::create_dir_all(&home).expect("home should be creatable");
        fs::write(
            home.join(".bash_profile"),
            "export PATH=\"$HOME/bin:$PATH\"\n",
        )
        .expect(".bash_profile should be writable");
        fs::write(
            home.join(".bash_login"),
            "source /usr/share/bash-completion/bash_completion\n",
        )
        .expect(".bash_login should be writable");
        fs::write(
            home.join(".profile"),
            "source /usr/share/bash-completion/bash_completion\n",
        )
        .expect(".profile should be writable");

        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("BASH_COMPLETION_VERSINFO")
            .with_existing_path("/usr/share/bash-completion/bash_completion")
            .without_real_path_lookups();

        let status = loader_status(&env).expect("status should resolve");

        assert_eq!(status, LoaderStatus::PresentButUnwired);
    }

    #[test]
    fn loader_status_ignores_comments_and_plain_strings() {
        let temp_root = crate::tests::temp_dir("bash-loader-ignore-comments");
        let home = temp_root.join("home");
        fs::create_dir_all(&home).expect("home should be creatable");
        fs::write(
            home.join(".profile"),
            "# source /usr/share/bash-completion/bash_completion\nBASH_LOADER=/usr/share/bash-completion/bash_completion\necho \"source /usr/share/bash-completion/bash_completion\"\n",
        )
        .expect(".profile should be writable");

        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("BASH_COMPLETION_VERSINFO")
            .with_existing_path("/usr/share/bash-completion/bash_completion")
            .without_real_path_lookups();

        let status = loader_status(&env).expect("status should resolve");

        assert_eq!(status, LoaderStatus::PresentButUnwired);
    }

    #[test]
    fn install_uses_system_loader_only_when_bashrc_wiring_is_detected() {
        let temp_root = crate::tests::temp_dir("bash-loader-install-wired");
        let home = temp_root.join("home");
        let target = home.join(".local/share/bash-completion/completions/tool");
        fs::create_dir_all(&home).expect("home should be creatable");
        fs::write(
            home.join(".bashrc"),
            "source /usr/share/bash-completion/bash_completion\n",
        )
        .expect(".bashrc should be writable");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("BASH_COMPLETION_VERSINFO")
            .with_existing_path("/usr/share/bash-completion/bash_completion")
            .without_real_path_lookups();

        let report = install(&env, "tool", &target).expect("install should succeed");

        assert_eq!(report.report.mode, ActivationMode::SystemLoader);
        assert_eq!(
            report.report.availability,
            Availability::AvailableAfterNewShell
        );
        assert_eq!(report.affected_locations, vec![home.join(".bashrc")]);
    }

    #[test]
    fn install_falls_back_to_managed_block_when_loader_file_is_not_wired() {
        let temp_root = crate::tests::temp_dir("bash-loader-install-fallback");
        let home = temp_root.join("home");
        let target = home.join(".local/share/bash-completion/completions/tool");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("BASH_COMPLETION_VERSINFO")
            .with_existing_path("/usr/share/bash-completion/bash_completion")
            .without_real_path_lookups();

        let report = install(&env, "tool", &target).expect("install should succeed");

        assert_eq!(report.report.mode, ActivationMode::ManagedRcBlock);
        assert_eq!(
            report.report.availability,
            Availability::AvailableAfterSource
        );
        let bashrc = fs::read_to_string(home.join(".bashrc")).expect(".bashrc should be created");
        assert!(bashrc.contains("shellcomp bash tool"));
    }

    #[test]
    fn install_quotes_bashrc_path_in_next_step_when_home_has_spaces() {
        let temp_root = crate::tests::temp_dir("bash-loader-install-next-step");
        let home = temp_root.join("home with space");
        let target = home.join(".local/share/bash-completion/completions/tool");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();

        let report = install(&env, "tool", &target).expect("install should succeed");

        let next_step = report.report.next_step.expect("next_step should exist");
        assert!(next_step.contains("source '"));
        assert!(next_step.contains("home with space/.bashrc"));
    }

    #[test]
    fn detect_requires_manual_action_when_file_missing() {
        let env = Environment::test()
            .without_var("BASH_COMPLETION_VERSINFO")
            .without_existing_path("/usr/share/bash-completion/bash_completion")
            .with_var("HOME", "/tmp/test-home")
            .without_real_path_lookups();

        let report =
            detect(&env, "tool", Path::new("/tmp/missing")).expect("detect should succeed");

        assert_eq!(report.mode, ActivationMode::ManagedRcBlock);
        assert_eq!(report.availability, Availability::ManualActionRequired);
    }

    #[test]
    fn detect_does_not_assume_system_loader_from_loader_file_alone() {
        let temp_root = crate::tests::temp_dir("bash-loader-detect-fallback");
        let home = temp_root.join("home");
        fs::create_dir_all(&home).expect("home should be creatable");

        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("BASH_COMPLETION_VERSINFO")
            .with_existing_path("/usr/share/bash-completion/bash_completion")
            .without_real_path_lookups();

        let report = detect(
            &env,
            "tool",
            &home.join(".local/share/bash-completion/completions/tool"),
        )
        .expect("detect should succeed");

        assert_eq!(report.mode, ActivationMode::ManagedRcBlock);
        assert_eq!(report.availability, Availability::ManualActionRequired);
    }

    #[test]
    fn detect_existing_completion_does_not_assume_system_loader_from_loader_file_alone() {
        let temp_root = crate::tests::temp_dir("bash-loader-detect-existing-fallback");
        let home = temp_root.join("home");
        let completion_path = home.join(".local/share/bash-completion/completions/tool");
        fs::create_dir_all(
            completion_path
                .parent()
                .expect("completion path should have a parent"),
        )
        .expect("completion dir should be creatable");
        fs::write(&completion_path, "complete -F _tool tool\n")
            .expect("completion file should be writable");

        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("BASH_COMPLETION_VERSINFO")
            .with_existing_path("/usr/share/bash-completion/bash_completion")
            .without_real_path_lookups();

        let report = detect(&env, "tool", &completion_path).expect("detect should succeed");

        assert_eq!(report.mode, ActivationMode::ManagedRcBlock);
        assert_eq!(report.availability, Availability::ManualActionRequired);
    }

    #[test]
    fn detect_reports_system_loader_when_bashrc_wires_loader_for_new_shells() {
        let temp_root = crate::tests::temp_dir("bash-loader-detect-wired");
        let home = temp_root.join("home");
        let completion_path = home.join(".local/share/bash-completion/completions/tool");
        fs::create_dir_all(
            completion_path
                .parent()
                .expect("completion path should have a parent"),
        )
        .expect("completion dir should be creatable");
        fs::write(&completion_path, "complete -F _tool tool\n")
            .expect("completion file should be writable");
        fs::write(
            home.join(".profile"),
            "source /usr/share/bash-completion/bash_completion\n",
        )
        .expect(".profile should be writable");

        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("BASH_COMPLETION_VERSINFO")
            .with_existing_path("/usr/share/bash-completion/bash_completion")
            .without_real_path_lookups();

        let report = detect(&env, "tool", &completion_path).expect("detect should succeed");

        assert_eq!(report.mode, ActivationMode::SystemLoader);
        assert_eq!(report.availability, Availability::AvailableAfterNewShell);
        assert_eq!(report.location, Some(home.join(".profile")));
    }

    #[test]
    fn detect_unwired_guidance_uses_actual_bashrc_and_completion_paths() {
        let temp_root = crate::tests::temp_dir("bash-detect-unwired-guidance");
        let home = temp_root.join("home with space");
        let completion_path = home.join(".local/share/bash-completion/completions/tool");
        fs::create_dir_all(
            completion_path
                .parent()
                .expect("completion path should have a parent"),
        )
        .expect("completion dir should be creatable");
        fs::write(&completion_path, "complete -F _tool tool\n")
            .expect("completion file should be writable");

        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();

        let report = detect(&env, "tool", &completion_path).expect("detect should succeed");

        assert_eq!(report.mode, ActivationMode::ManagedRcBlock);
        assert_eq!(report.availability, Availability::ManualActionRequired);
        let next_step = report.next_step.expect("next_step should exist");
        assert!(next_step.contains("source '"));
        assert!(next_step.contains("home with space/.bashrc"));
        assert!(
            next_step.contains("home with space/.local/share/bash-completion/completions/tool")
        );
    }

    #[test]
    fn detect_reports_corruption_when_duplicate_managed_block_is_malformed() {
        let temp_root = crate::tests::temp_dir("bash-detect-corrupt-duplicate");
        let home = temp_root.join("home");
        let completion_path = home.join(".local/share/bash-completion/completions/tool");
        fs::create_dir_all(
            completion_path
                .parent()
                .expect("completion path should have a parent"),
        )
        .expect("completion dir should be creatable");
        fs::write(&completion_path, "complete -F _tool tool\n")
            .expect("completion file should be writable");
        fs::write(
            home.join(".bashrc"),
            "# >>> shellcomp bash tool >>>\nif [ -f '/tmp/tool' ]; then\n  . '/tmp/tool'\nfi\n# <<< shellcomp bash tool <<<\n# >>> shellcomp bash tool >>>\n. '/tmp/other'\n",
        )
        .expect(".bashrc should be writable");

        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("BASH_COMPLETION_VERSINFO")
            .without_existing_path("/usr/share/bash-completion/bash_completion")
            .without_real_path_lookups();

        let error = detect(&env, "tool", &completion_path).expect_err("detect should fail");

        assert!(matches!(error, crate::Error::ManagedBlockMissingEnd { .. }));
    }

    #[test]
    fn detect_reports_manual_action_when_duplicate_managed_blocks_exist() {
        let temp_root = crate::tests::temp_dir("bash-detect-duplicate-managed");
        let home = temp_root.join("home");
        let completion_path = home.join(".local/share/bash-completion/completions/tool");
        fs::create_dir_all(
            completion_path
                .parent()
                .expect("completion path should have a parent"),
        )
        .expect("completion dir should be creatable");
        fs::write(&completion_path, "complete -F _tool tool\n")
            .expect("completion file should be writable");
        fs::write(
            home.join(".bashrc"),
            "# >>> shellcomp bash tool >>>\nif [ -f '/tmp/tool' ]; then\n  . '/tmp/tool'\nfi\n# <<< shellcomp bash tool <<<\n# >>> shellcomp bash tool >>>\nif [ -f '/tmp/tool' ]; then\n  . '/tmp/tool'\nfi\n# <<< shellcomp bash tool <<<\n",
        )
        .expect(".bashrc should be writable");

        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("BASH_COMPLETION_VERSINFO")
            .without_existing_path("/usr/share/bash-completion/bash_completion")
            .without_real_path_lookups();

        let report = detect(&env, "tool", &completion_path).expect("detect should succeed");

        assert_eq!(report.mode, ActivationMode::ManagedRcBlock);
        assert_eq!(report.availability, Availability::ManualActionRequired);
    }

    #[test]
    fn install_reports_active_now_when_loader_is_active() {
        let temp_root = crate::tests::temp_dir("bash-loader-install-active");
        let home = temp_root.join("home");
        let target = home.join(".local/share/bash-completion/completions/tool");
        let env = Environment::test()
            .with_var("HOME", &home)
            .with_var("BASH_COMPLETION_VERSINFO", "2")
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();

        let report = install(&env, "tool", &target).expect("install should succeed");

        assert_eq!(report.report.mode, ActivationMode::SystemLoader);
        assert_eq!(report.report.availability, Availability::ActiveNow);
        assert!(report.report.next_step.is_none());
    }

    #[test]
    fn custom_bash_path_uses_managed_block_even_when_loader_is_active() {
        let temp_root = crate::tests::temp_dir("bash-custom-path-managed");
        let home = temp_root.join("home");
        let target = temp_root.join("custom").join("tool.bash");
        let env = Environment::test()
            .with_var("HOME", &home)
            .with_var("BASH_COMPLETION_VERSINFO", "2")
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();

        let report = install(&env, "tool", &target).expect("install should succeed");

        assert_eq!(report.report.mode, ActivationMode::ManagedRcBlock);
        assert_eq!(
            report.report.availability,
            Availability::AvailableAfterSource
        );
    }

    #[test]
    fn install_errors_when_bashrc_is_not_writable() {
        let temp_root = crate::tests::temp_dir("install-bash-manual-fallback");
        let home = temp_root.join("home");
        std::fs::create_dir_all(home.join(".bashrc")).expect("directory should be creatable");
        let env = Environment::test()
            .with_var("HOME", &home)
            .without_var("XDG_DATA_HOME")
            .without_real_path_lookups();

        let error = install(
            &env,
            "tool",
            &home.join(".local/share/bash-completion/completions/tool"),
        )
        .expect_err("install should return an error");

        assert!(matches!(error, crate::Error::Io { .. }));
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;

    #[test]
    fn source_detection_respects_quotes_comments_and_execution_contexts() {
        let loader = "/usr/share/bash-completion/bash_completion";
        for line in [
            format!("echo ignored # ; source {loader}"),
            format!("echo 'ignored ; source {loader} ; ignored'"),
            format!("echo \"ignored && source {loader} ; ignored\""),
            format!("echo ignored \\; source {loader}"),
            format!("(source {loader})"),
            format!("source {loader} &"),
            format!("source {loader} | cat"),
            format!("echo $(source {loader})"),
        ] {
            assert!(!line_sources_loader(&line, loader), "misclassified: {line}");
        }
        for line in [
            format!("source '{loader}' # comment"),
            format!("[ -f '{loader}' ] && . \"{loader}\""),
            format!("echo ready; source {loader}"),
        ] {
            assert!(line_sources_loader(&line, loader), "missed: {line}");
        }
    }

    #[test]
    fn quoted_paths_keep_spaces_and_literal_home_variables() {
        let env = Environment::test().with_var("HOME", "/tmp/home with space");
        for target in ["\"$HOME/rc\"", "\"${HOME}/rc\"", "~/rc"] {
            assert_eq!(
                resolve_sourced_path(&env, target),
                Some(PathBuf::from("/tmp/home with space/rc"))
            );
        }
        assert_eq!(
            line_source_targets("source '/tmp/home with space/rc'"),
            vec!["'/tmp/home with space/rc'"]
        );
        for target in ["'$HOME/rc'", "'~/rc'", "\"~/rc\""] {
            assert_eq!(resolve_sourced_path(&env, target), None);
        }
    }

    #[test]
    fn dynamic_source_paths_do_not_count_as_literal_loader_chains() {
        for target in [
            "/etc/profile.d/$name.sh",
            "\"/etc/profile.d/$name.sh\"",
            "/etc/profile.d/[ab].sh",
            "/etc/profile.d/{a,b}.sh",
            "$HOME/$name.sh",
        ] {
            let literal_path = target.trim_matches('"').replace("$HOME", "/tmp/home");
            let env = Environment::test()
                .with_var("HOME", "/tmp/home")
                .with_file_contents("/etc/profile", format!("source {target}\n"))
                .with_file_contents(
                    &literal_path,
                    "source /usr/share/bash-completion/bash_completion\n",
                )
                .with_existing_path("/usr/share/bash-completion/bash_completion");
            assert_eq!(
                file_reaches_loader(&env, Path::new("/etc/profile"), &mut BTreeSet::new()).unwrap(),
                None,
                "misclassified {target}"
            );
        }
        let env = Environment::test().with_var("HOME", "/tmp/home with spaces");
        assert_eq!(resolve_sourced_path(&env, "$HOME/rc"), None);
        assert_eq!(resolve_sourced_path(&env, "${HOME}/rc"), None);
        for target in ["'/etc/profile.d/$name.sh'", "\"/etc/profile.d/[ab].sh\""] {
            assert_eq!(
                resolve_sourced_path(&env, target),
                Some(PathBuf::from(unquote_shell_token(target)))
            );
        }
    }

    #[test]
    fn directory_enumeration_requires_sourcing_the_loop_variable() {
        let directory = Path::new("/etc/profile.d");
        for contents in [
            "for i in /etc/profile.d/*.sh; do echo $i; done",
            "for i in /etc/profile.d/*.sh; do . '$i'; done",
            "run-parts /etc/profile.d",
            "for i in '/etc/profile.d/*.sh'; do . $i; done",
        ] {
            assert!(
                !sources_profile_directory(contents, directory),
                "misclassified: {contents}"
            );
        }
        assert!(sources_profile_directory(
            "for i in /etc/profile.d/*.sh; do\n . \"${i}\"\ndone",
            directory
        ));
    }

    #[test]
    fn install_falls_back_to_managed_wiring_when_source_only_appears_in_a_comment() {
        let home = crate::tests::temp_dir("bash-comment-fallback");
        let original = "echo keep # ; source /usr/share/bash-completion/bash_completion\n";
        std::fs::write(home.join(".bashrc"), original).unwrap();
        let env = Environment::test()
            .with_var("HOME", &home)
            .with_existing_path("/usr/share/bash-completion/bash_completion");
        let target = paths::default_install_path(&env, &Shell::Bash, "tool").unwrap();
        let result = install(&env, "tool", &target).unwrap();
        assert_eq!(result.report.mode, ActivationMode::ManagedRcBlock);
        let profile = std::fs::read_to_string(home.join(".bashrc")).unwrap();
        assert!(profile.starts_with(original));
        assert!(profile.contains("# >>> shellcomp bash tool >>>"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn source_classification_agrees_with_bash_for_literal_commands() {
        let root = crate::tests::temp_dir("bash-source-comparison");
        let loader = root.join("loader");
        std::fs::write(&loader, "shellcomp_loaded=yes\n").unwrap();
        let loader = loader.to_str().unwrap();
        for line in [
            format!("source '{loader}'"),
            format!("echo keep # ; source {loader}"),
            format!("echo 'keep ; source {loader} ; keep'"),
            format!("echo \"keep && source {loader} ; keep\""),
            format!("(source '{loader}')"),
            format!("source '{loader}' | cat"),
        ] {
            let output = std::process::Command::new("bash")
                .args(["--noprofile", "--norc", "-c"])
                .arg(format!(
                    "unset shellcomp_loaded\n{line}\n[[ ${{shellcomp_loaded-}} == yes ]]"
                ))
                .env_remove("BASH_ENV")
                .output()
                .unwrap();
            assert_eq!(
                line_sources_loader(&line, loader),
                output.status.success(),
                "{line}"
            );
        }
    }
}

#[cfg(test)]
mod traversal_tests {
    use super::*;

    #[test]
    fn long_source_chains_do_not_consume_the_call_stack() {
        std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(|| {
                let mut env = Environment::test().with_existing_path(BASH_LOADER_PATHS[0]);
                for index in 0..2048 {
                    env = env.with_file_contents(
                        format!("/shellcomp-test/{index}"),
                        format!("source /shellcomp-test/{}\n", index + 1),
                    );
                }
                env = env.with_file_contents(
                    "/shellcomp-test/2048",
                    format!("source {}\n", BASH_LOADER_PATHS[0]),
                );
                let found =
                    file_reaches_loader(&env, Path::new("/shellcomp-test/0"), &mut BTreeSet::new())
                        .unwrap();
                assert_eq!(found, Some(PathBuf::from("/shellcomp-test/2048")));
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn cyclic_source_graphs_terminate() {
        let env = Environment::test()
            .with_file_contents("/shellcomp-test/one", "source /shellcomp-test/two")
            .with_file_contents("/shellcomp-test/two", "source /shellcomp-test/one");
        let mut visited = BTreeSet::new();
        assert_eq!(
            file_reaches_loader(&env, Path::new("/shellcomp-test/one"), &mut visited).unwrap(),
            None
        );
        assert_eq!(visited.len(), 2);
    }
}

#[cfg(test)]
mod multiline_tests {
    use super::*;

    #[test]
    fn startup_discovery_ignores_multiline_data_and_keeps_later_commands() {
        let loader = BASH_LOADER_PATHS[0];
        for script in [
            format!("echo 'a multiline string\nsource {loader}\n'\n"),
            format!("cat <<'EOF'\nsource {loader}\nEOF\n"),
            format!("cat <<-EOF\n\tsource {loader}\n\tEOF\n"),
            format!("cat <<ONE <<\"TWO\"\nsource {loader}\nONE\nsource {loader}\nTWO\n"),
            format!("echo \\\nsource {loader}\n"),
        ] {
            let env = Environment::test()
                .with_existing_path(loader)
                .with_file_contents("/shellcomp-test/startup", &*script);
            assert_eq!(
                file_reaches_loader(
                    &env,
                    Path::new("/shellcomp-test/startup"),
                    &mut BTreeSet::new()
                )
                .unwrap(),
                None,
                "{script}"
            );
            let env = env.with_file_contents(
                "/shellcomp-test/startup",
                format!("{script}\nsource {loader}\n"),
            );
            assert_eq!(
                file_reaches_loader(
                    &env,
                    Path::new("/shellcomp-test/startup"),
                    &mut BTreeSet::new()
                )
                .unwrap(),
                Some(PathBuf::from("/shellcomp-test/startup"))
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn multiline_classification_agrees_with_bash_execution() {
        let root = tempfile::tempdir().unwrap();
        let loader = root.path().join("loader");
        std::fs::write(&loader, "shellcomp_loaded=yes\n").unwrap();
        let loader = loader.to_str().unwrap();
        for contents in [
            format!("echo 'literal\nsource {loader}\n'\n"),
            format!("cat <<'EOF'\nsource {loader}\nEOF\n"),
            format!("cat <<-EOF\n\tsource {loader}\n\tEOF\nsource {loader}\n"),
            format!("echo \\\nsource {loader}\n"),
        ] {
            let detected = command_lines(&contents)
                .iter()
                .any(|line| line_sources_loader(line, loader));
            let output = std::process::Command::new("bash")
                .args(["--noprofile", "--norc", "-c"])
                .arg(format!(
                    "unset shellcomp_loaded\n{contents}\n[[ ${{shellcomp_loaded-}} == yes ]]"
                ))
                .env_remove("BASH_ENV")
                .output()
                .unwrap();
            assert_eq!(detected, output.status.success(), "{contents}");
        }
    }
}
