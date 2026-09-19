use std::env;

use shellcomp::{InstallRequest, Shell, UninstallRequest, install, uninstall};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let demo_path = env::temp_dir().join(format!("demo-roundtrip-{}.fish", std::process::id()));
    let script = b"complete -c demo -f\n";

    let install_report = install(InstallRequest {
        shell: Shell::Fish,
        program_name: "demo",
        script,
        path_override: Some(demo_path.clone()),
    })?;

    let uninstall_report = uninstall(UninstallRequest {
        shell: Shell::Fish,
        program_name: "demo",
        path_override: Some(demo_path.clone()),
    })?;

    println!("Installed and removed a custom-path completion file.");
    println!("Path: {}", demo_path.display());
    println!("Install report:\n{install_report:#?}");
    println!("Uninstall report:\n{uninstall_report:#?}");
    Ok(())
}
