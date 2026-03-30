#![doc = include_str!("../README.md")]
#![doc(issue_tracker_base_url = "https://github.com/base/base/issues/")]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

mod cli;
mod node;

use reth_cli_util::allocator::{Allocator, new_allocator};
use reth_tracing as _;

#[global_allocator]
static ALLOC: Allocator = new_allocator();

fn main() {
    base_cli_utils::init_common!();
    base_reth_cli::init_reth!();

    let cli = base_cli_utils::parse_cli!(cli::Cli, |cmd: clap::Command| cmd.name("base"));
    if let Err(err) = cli.run() {
        eprintln!("Error: {err:?}");
        std::process::exit(1);
    }
}
