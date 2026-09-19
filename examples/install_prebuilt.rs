use std::env;

use shellcomp::{InstallRequest, Shell, install};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let script = br#"_demo_complete() {
    COMPREPLY=("hello" "world")
}
complete -F _demo_complete demo
"#;
    let demo_path = env::temp_dir().join(format!("demo-prebuilt-{}.bash", std::process::id()));
    let report = install(InstallRequest {
        shell: Shell::Bash,
        program_name: "demo",
        script,
        path_override: Some(demo_path.clone()),
    })?;

    println!("Installed a prebuilt Bash completion script without touching your shell profile.");
    println!("Path: {}", demo_path.display());
    println!("{report:#?}");
    Ok(())
}
