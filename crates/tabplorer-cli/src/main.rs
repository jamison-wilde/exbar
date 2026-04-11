use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "tabplorer", about = "Manage the Tabplorer Explorer extension")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Install the Explorer extension
    Install,
    /// Uninstall the Explorer extension
    Uninstall {
        /// Also delete the DLL and local data
        #[arg(long)]
        clean: bool,
    },
    /// Show installation status
    Status,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::Install => println!("Install not yet implemented"),
        Commands::Uninstall { clean } => {
            println!("Uninstall not yet implemented (clean={clean})")
        }
        Commands::Status => println!("Status not yet implemented"),
    }
}
