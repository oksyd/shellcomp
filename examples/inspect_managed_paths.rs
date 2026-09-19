use shellcomp::{Shell, default_install_path, detect_activation};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
        let path = default_install_path(shell.clone(), "demo")?;
        let activation = detect_activation(shell.clone(), "demo")?;

        println!("Shell: {shell}");
        println!("Managed path: {}", path.display());
        println!("Activation: {activation:#?}");
        println!();
    }

    Ok(())
}
