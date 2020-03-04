#[macro_use]
extern crate log;

extern crate nimiq_lib as nimiq;


use std::convert::TryFrom;
use std::time::Duration;

use futures::{future, Future, Stream, IntoFuture};
use tokio;
use tokio::timer::Interval;

use nimiq_blockchain_base::AbstractBlockchain;

use nimiq_consensus::{
    ConsensusProtocol as AbstractConsensusProtocol,
    NimiqConsensusProtocol,
    AlbatrossConsensusProtocol,
};

use nimiq::prelude::*;
use nimiq::extras::logging::{initialize_logging, log_error_cause_chain};
use nimiq::extras::deadlock::initialize_deadlock_detection;
use nimiq::extras::panic::initialize_panic_reporting;

use nimiq_metrics_server::{AbstractChainMetrics, AlbatrossChainMetrics, NimiqChainMetrics};
use nimiq_metrics_server::server;
use nimiq_rpc_server::handlers::{BlockchainNimiqHandler, BlockchainAlbatrossHandler, AbstractBlockchainHandler, Module};

use nimiq_primitives::networks::NetworkId;

fn main_inner() -> Result<(), Error> {
    // Initialize deadlock detection
    initialize_deadlock_detection();

    // Parse command line.
    let command_line = CommandLine::from_args();
    trace!("Command line: {:#?}", command_line);

    // Parse config file - this will obey the `--config` command line option.
    let config_file = ConfigFile::find(Some(&command_line))?;
    trace!("Config file: {:#?}", config_file);

    // Initialize logging with config values
    initialize_logging(Some(&command_line), Some(&config_file.log))?;

    // Initialize panic hook
    initialize_panic_reporting();

    // Create config builder and apply command line and config file
    // You usually want the command line to override config settings, so the order is important
    let mut builder: ClientConfigBuilder = ClientConfig::builder();
    builder.config_file(&config_file)?;
    builder.command_line(&command_line)?;

    // finalize config
    let config: ClientConfig = builder.build()?;
    debug!("Final configuration: {:#?}", config);

    // initialize client from config with the appropriate types
    match config.network {
        NetworkId::Test | NetworkId::Dev | NetworkId::Bounty | NetworkId::Dummy | NetworkId::Main => {
            initialize::<NimiqConsensusProtocol, BlockchainNimiqHandler, NimiqChainMetrics>(config, config_file)?;
        },
        NetworkId::TestAlbatross | NetworkId::DevAlbatross | NetworkId::UnitAlbatross => {
            initialize::<AlbatrossConsensusProtocol, BlockchainAlbatrossHandler, AlbatrossChainMetrics>(config, config_file)?;
        }
    };

    Ok(())
}

fn initialize<P: AbstractConsensusProtocol + BlockProducerFactory +'static, BH: AbstractBlockchainHandler<P::Blockchain> + Module, CM: AbstractChainMetrics<P> + server::Metrics + 'static>(config: ClientConfig, config_file: ConfigFile) -> Result<(), Error> {
    tokio::run(
        // TODO: Return this from `Client::into_future()`
        future::lazy(move || {
            // TODO: This is the initialization future

            // Clone those now, because we pass ownership of config to Client
            let protocol_config = config.protocol.clone();
            let rpc_config = config.rpc_server.clone();
            let metrics_config = config.metrics_server.clone();
            let ws_rpc_config = config.ws_rpc_server.clone();

            // initialize client
            let client = Client::<P>::try_from(config)?;
            client.initialize()?;

            // Initialize RPC server
            if let Some(rpc_config) = rpc_config {
                use nimiq::extras::rpc_server::initialize_rpc_server;
                let rpc_server = initialize_rpc_server::<P, BH>(&client, rpc_config)
                    .expect("Failed to initialize RPC server");
                tokio::spawn(rpc_server.into_future());
            }

            // Initialize metrics server
            if let Some(metrics_config) = metrics_config {
                use nimiq::config::config::ProtocolConfig;
                use nimiq::extras::metrics_server::initialize_metrics_server;
                if let ProtocolConfig::Wss{pkcs12_key_file, pkcs12_passphrase, .. } = protocol_config {
                    let pkcs12_key_file = pkcs12_key_file.to_str()
                        .unwrap_or_else(|| panic!("Failed to convert path to PKCS#12 key file to string: {}", pkcs12_key_file.display()));
                    let metrics_server = initialize_metrics_server::<P, CM>(&client, metrics_config, pkcs12_key_file, &pkcs12_passphrase)
                        .expect("Failed to initialize metrics server");
                    tokio::spawn(metrics_server.into_future());
                } else {
                    error!("Cannot provide metrics when running without a certificate");
                }
            }

            // Initialize Websocket RPC server
            // TODO: Configuration
            if let Some(ws_rpc_config) = ws_rpc_config {
                use nimiq::extras::ws_rpc_server::initialize_ws_rcp_server;
                let ws_rpc_server = initialize_ws_rcp_server(&client, ws_rpc_config)
                    .expect("Failed to initialize websocket RPC server");
                tokio::spawn(ws_rpc_server.into_future());
            }

            // Initialize network stack and connect
            info!("Connecting to network");

            client.connect()?;

            // The Nimiq client is now running and we can access it trough the `client` object.

            // TODO: RPC server and metrics server need to be instantiated here
            Ok(client)
        })

        .and_then(move |client: Client<P>| {
            // NOTE: This is the "monitor" future, which keeps the Client object alive.

            let mut statistics_interval = config_file.log.statistics;
            let mut show_statistics = true;
            if statistics_interval == 0 {
                statistics_interval = 10;
                show_statistics = false;
            }

            // Run this periodically and optionally show some info
            Interval::new_interval(Duration::from_secs(statistics_interval))
                .map_err(|e| panic!("Timer failed: {}", e))
                .for_each(move |_| {

                    if show_statistics {
                        let peer_count = client.network().connections.peer_count();
                        info!("Head: #{} - {}, Peers: {}",
                            client.blockchain().head_height(),
                            client.blockchain().head_hash(),
                            peer_count);
                    }

                    future::ok::<(), Error>(())
                })
        })
        .map_err(|e: Error| warn!("{}", e)));

    Ok(())
}

fn main() {
    if let Err(e) = main_inner() {
        log_error_cause_chain(&e);
    }
}
