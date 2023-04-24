use std::time::Duration;

use clap::Parser;
use color_eyre::eyre::Result;
use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use log::{error, info};
use tokio::sync::mpsc;
use tokio::{signal, time};

use astria_conductor::alert::Alert;
use astria_conductor::cli::Cli;
use astria_conductor::config::Config;
use astria_conductor::driver::{Driver, DriverCommand};
use astria_conductor::logger;

#[tokio::main]
async fn main() -> Result<()> {
    run().await?;
    Ok(())
}

async fn run() -> Result<()> {
    let args = Cli::parse();
    // logs
    logger::initialize(&args.log_level);

    // hierarchical config. cli args override Envars which override toml config values
    let conf: Config = Figment::new()
        .merge(Toml::file("ConductorConfig.toml"))
        .merge(Env::prefixed("ASTRIA_"))
        .merge(Serialized::defaults(args))
        .extract()?;

    log::info!("Using Celestia node at {}", conf.celestia_node_url);
    log::info!("Using execution node at {}", conf.execution_rpc_url);

    // spawn our driver
    let (alert_tx, mut alert_rx) = mpsc::unbounded_channel();
    let mut driver = Driver::new(conf, alert_tx).await?;
    let driver_tx = driver.cmd_tx.clone();

    tokio::task::spawn(async move {
        if let Err(e) = driver.run().await {
            panic!("Driver error: {}", e)
        }
    });

    // NOTE - this will most likely be replaced by an RPC server that will receive gossip
    //  messages from the sequencer
    let mut interval = time::interval(Duration::from_secs(3));

    let mut run = true;
    while run {
        tokio::select! {
            // handle alerts from the driver
            Some(alert) = alert_rx.recv() => {
                match alert {
                    Alert::DriverError(error_string) => {
                        error!("error: {}", error_string);
                        run = false;
                    }
                    Alert::BlockReceived{block_height} => {
                        info!("block received from DA layer; DA layer height: {}", block_height);
                    }
                }
            }
            // request new blocks every X seconds
            _ = interval.tick() => {
                if let Some(e) = driver_tx.send(DriverCommand::GetNewBlocks).err() {
                    // the only error that can happen here is SendError which occurs
                    // if the driver's receiver channel is dropped
                    error!("error sending GetNewBlocks command to driver: {}", e);
                    run = false;
                }
            }
            // shutdown properly on ctrl-c
            _ = signal::ctrl_c() => {
                if let Some(e) = driver_tx.send(DriverCommand::Shutdown).err() {
                    error!("error sending Shutdown command to driver: {}", e);
                }
                run = false;
            }
        }

        if !run {
            break;
        }
    }

    Ok(())
}