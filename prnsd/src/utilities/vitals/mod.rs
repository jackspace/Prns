mod json;

use std::path::PathBuf;

use clap::Args;

use super::configuration::LoadedConfiguration;

/// Full per-interface counters, straight out of the running daemon.
///
/// `status` answers what an RNS operator asks: is the link up, how many bytes moved. It
/// cannot answer whether a frame arrived and was discarded, because the stock report has
/// nowhere to carry the frame split. This command exists for that question, and it prints
/// one JSON object per run so a cron entry can build a JSONL timeline: an event at 03:00
/// still has to be provable at 06:00, and a live pane cannot do that.
#[derive(Clone, Debug, PartialEq, Eq, Args)]
pub struct VitalsArgs {
    #[arg(
        long,
        value_name = "DIR",
        help = "Use an alternate Reticulum config directory"
    )]
    pub config: Option<PathBuf>,

    #[arg(
        long,
        help = "Emit one JSON object (the default, and the only format today)"
    )]
    pub json: bool,

    #[arg(
        long,
        value_name = "SECONDS",
        default_value = "5",
        help = "How long to wait for the daemon to answer"
    )]
    pub timeout: u64,
}

pub async fn run(args: VitalsArgs) -> Result<(), String> {
    let configuration =
        LoadedConfiguration::load(args.config.as_deref()).map_err(|error| error.to_string())?;
    let client = configuration
        .local_rpc_client(std::time::Duration::from_secs(args.timeout.max(1)))
        .map_err(|error| error.to_string())?;
    let report = client
        .interface_vitals()
        .await
        .map_err(|error| error.to_string())?;
    println!(
        "{}",
        json::render(&report).map_err(|error| error.to_string())?
    );
    Ok(())
}
