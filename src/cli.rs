use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Forward a port through your router while this program is running."
)]
pub struct Args {
    /// Port used by your application on this device
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
    pub device_port: Option<u16>,
    /// Port friends connect to (0 uses the device port)
    #[arg(long)]
    pub router_port: Option<u16>,
    /// Load and optionally save a specific configuration file
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Review ports interactively, using saved settings as defaults
    #[arg(long, conflicts_with = "non_interactive")]
    pub interactive: bool,
    /// Save the selected settings for future launches
    #[arg(long)]
    pub save_config: bool,
    /// Never prompt or open a terminal (for scripts and services)
    #[arg(long)]
    pub non_interactive: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_conflicting_modes_and_invalid_ports() {
        assert!(Args::try_parse_from(["app", "--interactive", "--non-interactive"]).is_err());
        assert!(Args::try_parse_from(["app", "--device-port", "0"]).is_err());
        assert!(Args::try_parse_from(["app", "--router-port", "65536"]).is_err());
        assert_eq!(
            Args::try_parse_from(["app", "--router-port", "0"])
                .unwrap()
                .router_port,
            Some(0)
        );
    }
}
