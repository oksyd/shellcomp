use std::env;

use clap::Parser;
use shellcomp::{InstallRequest, install, render_clap_completion};

#[derive(Parser)]
struct Cli {
    #[arg(long)]
    verbose: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let generator_shell = shellcomp::clap_complete::Shell::Bash;
    let script = render_clap_completion::<Cli>(generator_shell, "example-cli")?;
    let demo_path = env::temp_dir().join(format!("example-cli-{}.bash", std::process::id()));
    let report = install(InstallRequest {
        shell: generator_shell.into(),
        program_name: "example-cli",
        script: &script,
        path_override: Some(demo_path.clone()),
    })?;

    println!("Rendered completion from clap and installed it to a temporary path.");
    println!("Path: {}", demo_path.display());
    println!("{report:#?}");
    Ok(())
}
