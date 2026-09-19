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
