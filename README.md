# shellcomp

`shellcomp` is a deployment layer for shell completions in Rust CLI projects.

It does not generate completion scripts. It installs, wires, detects, and removes them in a way
that is predictable, idempotent, and structured for callers.

## What It Handles

- Default install paths for Bash, Zsh, Fish, PowerShell, and Elvish
- `write-if-changed` completion file writes
- Managed `~/.bashrc` fallback wiring when Bash has no system loader
- Managed `~/.zshrc` wiring for `fpath` and `compinit`
- Native Fish completion directory installs
- Managed PowerShell profile wiring
- Managed Elvish `rc.elv` wiring
- Symmetric uninstall cleanup
- Legacy managed-block migration helpers
- Structured reports that callers can render however they want

## Supported Shells

- Bash
- Zsh
- Fish
- PowerShell
- Elvish

`Shell::Other(_)` remains the explicit unsupported-shell escape hatch.

## Add The Dependency

Until the crate is published, depend on a local checkout or a git revision:

```toml
[dependencies]
shellcomp = { path = "../shellcomp-rs" }
```

Once published, replace the path dependency with a version:

```toml
[dependencies]
shellcomp = "0.1.0"
```

Enable `clap` integration if you want the library to render completions from
`clap::CommandFactory` or a prebuilt `clap::Command` directly:

```toml
[dependencies]
shellcomp = { version = "0.1.0", features = ["clap"] }
clap = { version = "4.6.0", features = ["derive"] }
```

## Install A Prebuilt Completion Script

```rust
use shellcomp::{InstallRequest, Shell, install};

fn install_bash_completion(script: &[u8]) -> shellcomp::Result<()> {
    let report = install(InstallRequest {
        shell: Shell::Bash,
        program_name: "my-cli",
        script,
        path_override: None,
    })?;

    println!("{report:#?}");
    Ok(())
}
```

## Integrate With clap

```rust
use clap::Parser;
use shellcomp::{InstallRequest, install, render_clap_completion};

#[derive(Parser)]
struct Cli {
    #[arg(long)]
    verbose: bool,
}

fn install_zsh_completion() -> Result<(), Box<dyn std::error::Error>> {
    let generator_shell = shellcomp::clap_complete::Shell::Zsh;
    let script = render_clap_completion::<Cli>(generator_shell, "my-cli")?;
    let report = install(InstallRequest {
        shell: generator_shell.into(),
        program_name: "my-cli",
        script: &script,
        path_override: None,
    })?;

    println!("{report:#?}");
    Ok(())
}
```

If you want to avoid `Shell` naming conflicts, use the re-exported shell type for generation and
convert it into `shellcomp::Shell` only when you need deployment:

```rust
use shellcomp::clap_complete::Shell;
```

If you need to tweak the command tree before rendering, use
`render_clap_completion_from_command`:

```rust
use clap::{Arg, CommandFactory, Parser};
use shellcomp::render_clap_completion_from_command;

#[derive(Parser)]
struct Cli {
    #[arg(long)]
    verbose: bool,
}

let command = Cli::command().arg(Arg::new("profile").long("profile"));
let script = render_clap_completion_from_command(
    shellcomp::clap_complete::Shell::Fish,
    "my-cli",
    command,
)?;
```

If you need lower-level generator APIs such as `generate`, depend on `clap_complete` directly.

The examples above install into managed shell locations. Use `path_override` during local testing if
you do not want to touch your real shell profile yet.

## Uninstall

```rust
use shellcomp::{Shell, UninstallRequest, uninstall};

fn remove_fish_completion() -> shellcomp::Result<()> {
    let report = uninstall(UninstallRequest {
        shell: Shell::Fish,
        program_name: "my-cli",
        path_override: None,
    })?;

    println!("{report:#?}");
    Ok(())
}
```

## Custom Paths

When `path_override` is set, `install` keeps the legacy behavior for non-default custom paths and
reports `ActivationMode::Manual`. If the override is exactly the shell's managed default path, the
default activation semantics still apply. If you want a custom path plus managed Bash/Zsh/PowerShell/Elvish
activation, use `install_with_policy(..., ActivationPolicy::AutoManaged)`.

```rust
use std::path::PathBuf;

use shellcomp::{InstallRequest, Shell, install};

fn install_to_custom_path(script: &[u8]) -> shellcomp::Result<()> {
    let report = install(InstallRequest {
        shell: Shell::Bash,
        program_name: "my-cli",
        script,
        path_override: Some(PathBuf::from("/tmp/my-cli.bash")),
    })?;

    assert_eq!(report.activation.mode, shellcomp::ActivationMode::Manual);
    Ok(())
}
```

## Legacy Block Migration

If your CLI previously shipped its own managed markers, rewrite them into `shellcomp` markers
before or during migration:

```rust
use shellcomp::{LegacyManagedBlock, MigrateManagedBlocksRequest, Shell, migrate_managed_blocks};

fn migrate_old_bash_block() -> shellcomp::Result<()> {
    let report = migrate_managed_blocks(MigrateManagedBlocksRequest {
        shell: Shell::Bash,
        program_name: "my-cli",
        path_override: None,
        legacy_blocks: vec![LegacyManagedBlock {
            start_marker: "# >>> my-cli completion >>>".to_owned(),
            end_marker: "# <<< my-cli completion <<<".to_owned(),
        }],
    })?;

    println!("{report:#?}");
    Ok(())
}
```

## Examples

- `cargo run --example install_prebuilt`
- `cargo run --example roundtrip_custom_path`
- `cargo run --example inspect_managed_paths`
- `cargo run --example clap_integration --features clap`
