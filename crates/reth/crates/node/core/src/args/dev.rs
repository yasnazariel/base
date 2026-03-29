//! clap [Args](clap::Args) for Dev testnet configuration

use clap::Args;

const DEFAULT_MNEMONIC: &str = "test test test test test test test test test test test junk";

/// Parameters for Dev testnet configuration
#[derive(Debug, Args, PartialEq, Eq, Clone)]
#[command(next_help_heading = "Dev testnet")]
pub struct DevArgs {
    /// Start the node in dev mode
    ///
    /// This mode enables development-oriented defaults.
    /// Disables network discovery and enables local http server.
    /// Prefunds 20 accounts derived by mnemonic "test test test test test test test test test test
    /// test junk" with 10 000 ETH each.
    #[arg(long = "dev", help_heading = "Dev testnet", verbatim_doc_comment)]
    pub dev: bool,

    /// Derive dev accounts from a fixed mnemonic instead of random ones.
    #[arg(
        long = "dev.mnemonic",
        help_heading = "Dev testnet",
        value_name = "MNEMONIC",
        requires = "dev",
        verbatim_doc_comment,
        default_value = DEFAULT_MNEMONIC
    )]
    pub dev_mnemonic: String,
}

impl Default for DevArgs {
    fn default() -> Self {
        Self { dev: false, dev_mnemonic: DEFAULT_MNEMONIC.to_string() }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    /// A helper type to parse Args more easily
    #[derive(Parser)]
    struct CommandParser<T: Args> {
        #[command(flatten)]
        args: T,
    }

    #[test]
    fn test_parse_dev_args() {
        let args = CommandParser::<DevArgs>::parse_from(["reth"]).args;
        assert_eq!(
            args,
            DevArgs { dev: false, dev_mnemonic: DEFAULT_MNEMONIC.to_string() }
        );

        let args = CommandParser::<DevArgs>::parse_from(["reth", "--dev"]).args;
        assert_eq!(
            args,
            DevArgs { dev: true, dev_mnemonic: DEFAULT_MNEMONIC.to_string() }
        );
    }

    #[test]
    fn dev_args_default_sanity_check() {
        let default_args = DevArgs::default();
        let args = CommandParser::<DevArgs>::parse_from(["reth"]).args;
        assert_eq!(args, default_args);
    }
}
