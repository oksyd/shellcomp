use std::borrow::Cow;
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};
use crate::infra::env::Environment;
use crate::model::Shell;

pub(crate) fn validate_program_name(program_name: &str) -> Result<()> {
    if program_name.is_empty() {
        return Err(Error::EmptyProgramName);
    }

    let is_safe = program_name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'));

    if !is_safe || program_name == "." || program_name == ".." {
        return Err(Error::InvalidProgramName {
            program_name: program_name.to_owned(),
        });
    }

    Ok(())
}

pub(crate) fn default_install_path(
    env: &Environment,
    shell: &Shell,
    program_name: &str,
) -> Result<PathBuf> {
    validate_program_name(program_name)?;

    match shell {
        Shell::Bash => Ok(env
            .xdg_data_home()?
            .join("bash-completion")
            .join("completions")
            .join(program_name)),
        Shell::Zsh => Ok(env
            .zdotdir()?
            .join(".zfunc")
            .join(format!("_{program_name}"))),
        Shell::Fish => Ok(env
            .xdg_config_home()?
            .join("fish")
            .join("completions")
            .join(format!("{program_name}.fish"))),
        Shell::Powershell => Ok(env
            .powershell_default_install_dir()?
            .join(format!("{program_name}.ps1"))),
        Shell::Elvish => Ok(env
            .xdg_config_home()?
            .join("elvish")
            .join("lib")
            .join("shellcomp")
            .join(format!("{program_name}.elv"))),
        unsupported => Err(Error::UnsupportedShell(unsupported.clone())),
    }
}

pub(crate) fn startup_path(env: &Environment, shell: &Shell) -> Result<Option<PathBuf>> {
    Ok(match shell {
        Shell::Bash => Some(env.home_dir()?.join(".bashrc")),
        Shell::Zsh => Some(env.zdotdir()?.join(".zshrc")),
        Shell::Powershell => Some(env.powershell_profile_path()?),
        Shell::Elvish => Some(env.xdg_config_home()?.join("elvish").join("rc.elv")),
        Shell::Fish | Shell::Other(_) => None,
    })
}

// macOS exposes these system directories through aliases. Keep user-facing paths,
// but use one identity for collision checks and in-process locks.
#[cfg(any(target_os = "macos", test))]
const MACOS_DIRECTORY_ALIASES: [(&str, &str); 3] = [
    ("/var", "/private/var"),
    ("/tmp", "/private/tmp"),
    ("/etc", "/private/etc"),
];

pub(crate) fn path_identity(path: &Path) -> Cow<'_, Path> {
    #[cfg(target_os = "macos")]
    {
        macos_path_identity(path)
    }
    #[cfg(not(target_os = "macos"))]
    Cow::Borrowed(path)
}

#[cfg(any(target_os = "macos", test))]
fn macos_path_identity(path: &Path) -> Cow<'_, Path> {
    for (alias, destination) in MACOS_DIRECTORY_ALIASES {
        if let Ok(suffix) = path.strip_prefix(alias) {
            return Cow::Owned(Path::new(destination).join(suffix));
        }
    }
    Cow::Borrowed(path)
}

fn is_system_directory_alias(path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    for (alias, destination) in MACOS_DIRECTORY_ALIASES {
        if path == Path::new(alias) {
            return fs::read_link(path)
                .is_ok_and(|link| Path::new("/").join(link) == Path::new(destination));
        }
    }
    let _ = path;
    false
}

pub(crate) fn validate_target_path(path: &Path) -> Result<()> {
    if path.is_relative() {
        return Err(Error::InvalidTargetPath {
            path: path.to_path_buf(),
            reason: "target path must be absolute",
        });
    }

    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(Error::InvalidTargetPath {
            path: path.to_path_buf(),
            reason: "target path must be normalized",
        });
    }

    for candidate in path.ancestors() {
        match fs::symlink_metadata(candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                if candidate != path && is_system_directory_alias(candidate) {
                    // Inspect the physical path too; descendants may still be user symlinks.
                    validate_target_path(path_identity(path).as_ref())?;
                    continue;
                }
                return Err(Error::InvalidTargetPath {
                    path: path.to_path_buf(),
                    reason: "target path must not be a symbolic link",
                });
            }
            Ok(metadata) if candidate == path && !metadata.is_file() && !metadata.is_dir() => {
                return Err(Error::InvalidTargetPath {
                    path: path.to_path_buf(),
                    reason: "target path must be a regular file",
                });
            }
            Ok(metadata) if metadata.is_file() && candidate == path => {}
            Ok(metadata) if !metadata.is_dir() => {
                return Err(Error::InvalidTargetPath {
                    path: path.to_path_buf(),
                    reason: "target path parent is not a directory",
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotADirectory => {
                // `NotADirectory` can occur when an ancestor segment is not a real directory.
                return Err(Error::InvalidTargetPath {
                    path: path.to_path_buf(),
                    reason: "target path parent is not a directory",
                });
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(Error::io("inspect path", candidate, error));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{default_install_path, validate_program_name};
    use crate::infra::env::Environment;
    use crate::model::Shell;

    #[test]
    fn resolves_default_paths() {
        let env = Environment::test()
            .with_var("HOME", "/tmp/home")
            .without_var("XDG_DATA_HOME")
            .without_var("XDG_CONFIG_HOME")
            .without_var("ZDOTDIR");

        assert_eq!(
            default_install_path(&env, &Shell::Bash, "tool").expect("bash path should resolve"),
            std::path::PathBuf::from("/tmp/home/.local/share/bash-completion/completions/tool")
        );
        assert_eq!(
            default_install_path(&env, &Shell::Zsh, "tool").expect("zsh path should resolve"),
            std::path::PathBuf::from("/tmp/home/.zfunc/_tool")
        );
        assert_eq!(
            default_install_path(&env, &Shell::Fish, "tool").expect("fish path should resolve"),
            std::path::PathBuf::from("/tmp/home/.config/fish/completions/tool.fish")
        );
        assert_eq!(
            default_install_path(&env, &Shell::Powershell, "tool")
                .expect("powershell path should resolve"),
            std::path::PathBuf::from("/tmp/home/.local/share/powershell/completions/tool.ps1")
        );
        assert_eq!(
            default_install_path(&env, &Shell::Elvish, "tool").expect("elvish path should resolve"),
            std::path::PathBuf::from("/tmp/home/.config/elvish/lib/shellcomp/tool.elv")
        );
    }

    #[test]
    fn honors_xdg_and_zdotdir_overrides() {
        let env = Environment::test()
            .with_var("HOME", "/tmp/home")
            .with_var("XDG_DATA_HOME", "/tmp/data")
            .with_var("XDG_CONFIG_HOME", "/tmp/config")
            .with_var("ZDOTDIR", "/tmp/zdotdir");

        assert_eq!(
            default_install_path(&env, &Shell::Bash, "tool").expect("bash path should resolve"),
            std::path::PathBuf::from("/tmp/data/bash-completion/completions/tool")
        );
        assert_eq!(
            default_install_path(&env, &Shell::Zsh, "tool").expect("zsh path should resolve"),
            std::path::PathBuf::from("/tmp/zdotdir/.zfunc/_tool")
        );
        assert_eq!(
            default_install_path(&env, &Shell::Fish, "tool").expect("fish path should resolve"),
            std::path::PathBuf::from("/tmp/config/fish/completions/tool.fish")
        );
        assert_eq!(
            default_install_path(&env, &Shell::Powershell, "tool")
                .expect("powershell path should resolve"),
            std::path::PathBuf::from("/tmp/data/powershell/completions/tool.ps1")
        );
        assert_eq!(
            default_install_path(&env, &Shell::Elvish, "tool").expect("elvish path should resolve"),
            std::path::PathBuf::from("/tmp/config/elvish/lib/shellcomp/tool.elv")
        );
    }

    #[test]
    fn resolves_windows_style_powershell_path() {
        let env = Environment::test()
            .with_windows_platform()
            .with_var("USERPROFILE", r"C:\Users\demo")
            .without_var("HOME")
            .without_var("XDG_DATA_HOME");

        assert_eq!(
            default_install_path(&env, &Shell::Powershell, "tool")
                .expect("powershell path should resolve"),
            std::path::PathBuf::from(r"C:\Users\demo")
                .join("Documents")
                .join("PowerShell")
                .join("Completions")
                .join("tool.ps1")
        );
    }

    #[test]
    fn rejects_invalid_program_names() {
        for invalid in [
            "",
            ".",
            "..",
            "dir/tool",
            "dir\\tool",
            "two words",
            "bad\nname",
        ] {
            assert!(validate_program_name(invalid).is_err());
        }
    }
}

#[cfg(test)]
mod system_alias_tests {
    use super::*;

    #[test]
    fn macos_system_aliases_have_a_single_component_aware_identity() {
        for (alias, destination) in MACOS_DIRECTORY_ALIASES {
            let original = Path::new(alias).join("nested/missing/file");
            let physical = Path::new(destination).join("nested/missing/file");
            assert_eq!(macos_path_identity(&original).as_ref(), physical);
            assert_eq!(macos_path_identity(&physical).as_ref(), physical);
        }
        for path in [
            "/variable/file",
            "/tmp-other/file",
            "/etcetera/file",
            "/home/user/link/file",
        ] {
            assert_eq!(
                macos_path_identity(Path::new(path)).as_ref(),
                Path::new(path)
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_temp_paths_allow_system_aliases_but_reject_user_symlinks() {
        let root = tempfile::tempdir_in("/var/tmp").unwrap();
        let physical = root.path().canonicalize().unwrap();
        let alias = Path::new("/").join(physical.strip_prefix("/private").unwrap());
        let env = Environment::test().with_var("HOME", &alias);
        let profile = physical.join(".bashrc");
        assert!(matches!(
            crate::service::resolve_target_path(&env, &Shell::Bash, "tool", Some(&profile)),
            Err(Error::InvalidTargetPath { .. })
        ));
        let default = default_install_path(&env, &Shell::Bash, "tool").unwrap();
        assert!(crate::service::default_target_path_matches(
            &env,
            &Shell::Bash,
            "tool",
            path_identity(&default).as_ref()
        ));
        let missing = alias.join("not-created/target");
        validate_target_path(&missing).unwrap();
        assert_eq!(
            path_identity(&missing).as_ref(),
            physical.join("not-created/target")
        );
        let target = alias.join("target");
        super::super::fs::write_if_changed(&target, b"data").unwrap();
        assert_eq!(fs::read(physical.join("target")).unwrap(), b"data");
        std::os::unix::fs::symlink(&target, alias.join("link")).unwrap();
        assert!(validate_target_path(&alias.join("link")).is_err());
        std::os::unix::fs::symlink(&physical, alias.join("directory-link")).unwrap();
        assert!(validate_target_path(&alias.join("directory-link/child")).is_err());
        super::super::fs::remove_file_if_exists(&target).unwrap();
    }
}
