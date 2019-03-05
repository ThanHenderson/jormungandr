mod bootstrap;
mod client;
mod server;

use super::NetworkBlockConfig;
use crate::{blockchain::BlockchainR, settings::start::network::Peer};

pub use self::client::run_connect_socket;
pub use self::server::run_listen_socket;

use chain_core::property;
use network_grpc::peer::TcpPeer;

pub fn bootstrap_from_peer<B>(peer: Peer, blockchain: BlockchainR<B>)
where
    B: NetworkBlockConfig,
    <B::Ledger as property::Ledger>::Update: Clone,
    <B::Settings as property::Settings>::Update: Clone,
    <B::Leader as property::LeaderSelection>::Update: Clone,
{
    info!("connecting to bootstrap peer {}", peer.connection);
    let peer = TcpPeer::new(*peer.address());
    bootstrap::bootstrap_from_target(peer, blockchain)
}
