mod bash;
mod elvish;
mod fish;
mod powershell;
mod zsh;

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::infra::env::Environment;
use crate::infra::managed_block::{self, ManagedBlock};
use crate::model::{ActivationReport, CleanupReport, FileChange, LegacyManagedBlock, Shell};

#[derive(Debug)]
pub(crate) struct ActivationOutcome {
    pub(crate) report: ActivationReport,
    pub(crate) affected_locations: Vec<PathBuf>,
}

#[derive(Debug)]
pub(crate) struct CleanupOutcome {
    pub(crate) cleanup: CleanupReport,
    pub(crate) affected_locations: Vec<PathBuf>,
}

#[derive(Debug)]
pub(crate) struct MigrationOutcome {
    pub(crate) location: Option<PathBuf>,
    pub(crate) managed_change: FileChange,
    pub(crate) legacy_change: FileChange,
    pub(crate) affected_locations: Vec<PathBuf>,
}

pub(crate) fn install_default(
    env: &Environment,
    shell: &Shell,
    program_name: &str,
    target_path: &Path,
) -> Result<ActivationOutcome> {
    match shell {
        Shell::Bash => bash::install(env, program_name, target_path),
        Shell::Zsh => zsh::install(env, program_name, target_path),
        Shell::Fish => fish::install(program_name, target_path),
        Shell::Powershell => powershell::install(env, program_name, target_path),
        Shell::Elvish => elvish::install(env, program_name, target_path),
        unsupported => Err(crate::Error::UnsupportedShell(unsupported.clone())),
    }
}

pub(crate) fn uninstall_default(
    env: &Environment,
    shell: &Shell,
    program_name: &str,
    target_path: &Path,
) -> Result<CleanupOutcome> {
    match shell {
        Shell::Bash => bash::uninstall(env, program_name, target_path),
        Shell::Zsh => zsh::uninstall(env, program_name, target_path),
        Shell::Fish => fish::uninstall(program_name, target_path),
        Shell::Powershell => powershell::uninstall(env, program_name, target_path),
        Shell::Elvish => elvish::uninstall(env, program_name, target_path),
        unsupported => Err(crate::Error::UnsupportedShell(unsupported.clone())),
    }
}

pub(crate) fn detect_default(
    env: &Environment,
    shell: &Shell,
    program_name: &str,
    target_path: &Path,
) -> Result<ActivationReport> {
    match shell {
        Shell::Bash => bash::detect(env, program_name, target_path),
        Shell::Zsh => zsh::detect(env, program_name, target_path),
        Shell::Fish => fish::detect(program_name, target_path),
        Shell::Powershell => powershell::detect(env, program_name, target_path),
        Shell::Elvish => elvish::detect(env, program_name, target_path),
        unsupported => Err(crate::Error::UnsupportedShell(unsupported.clone())),
    }
}

pub(crate) fn migrate_managed_blocks(
    env: &Environment,
    shell: &Shell,
    program_name: &str,
    target_path: &Path,
    legacy_blocks: &[LegacyManagedBlock],
) -> Result<MigrationOutcome> {
    match shell {
        Shell::Bash => bash::migrate(env, program_name, target_path, legacy_blocks),
        Shell::Zsh => zsh::migrate(env, program_name, target_path, legacy_blocks),
        Shell::Fish => Ok(MigrationOutcome {
            location: None,
            managed_change: FileChange::Absent,
            legacy_change: FileChange::Absent,
            affected_locations: Vec::new(),
        }),
        Shell::Powershell => powershell::migrate(env, program_name, target_path, legacy_blocks),
        Shell::Elvish => elvish::migrate(env, program_name, target_path, legacy_blocks),
        unsupported => Err(crate::Error::UnsupportedShell(unsupported.clone())),
    }
}

pub(crate) fn migrate_profile_blocks(
    profile_path: &Path,
    legacy_blocks: &[LegacyManagedBlock],
    managed_block: &ManagedBlock,
) -> Result<(FileChange, FileChange)> {
    let blocks: Vec<_> = legacy_blocks
        .iter()
        .map(|legacy| ManagedBlock {
            start_marker: legacy.start_marker.clone(),
            end_marker: legacy.end_marker.clone(),
            body: String::new(),
        })
        .collect();

    managed_block::migrate_blocks(profile_path, &blocks, managed_block)
}
