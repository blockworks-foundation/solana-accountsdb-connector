extern crate core;

use std::env;
use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::ops::{Div, Mul};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use arrayref::array_ref;
use futures_channel::mpsc::{unbounded, UnboundedSender};
use futures_util::{future, pin_mut, SinkExt, StreamExt, TryStreamExt};
use itertools::Itertools;
use log::info;
use mango::matching::{AnyNode, BookSide, LeafNode, NodeHandle};
use mango::queue::{AnyEvent, EventQueueHeader, EventType, FillEvent};
use serde::{Serialize, Serializer};
use serde::ser::SerializeStruct;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::{Error, protocol::Message};

use {
    log::*,
    serde::Deserialize,
    solana_geyser_connector_lib::{*, chain_data::*},
    solana_sdk::{account::{ReadableAccount, WritableAccount}, clock::Epoch, pubkey::Pubkey},
    std::{fs::File, io::Read, mem::size_of, str::FromStr},
};

#[derive(Clone, Debug, Deserialize)]
pub struct Spec {
    pub market: String,
    pub base_decimals: i64,
    pub quote_decimals: i64,
    pub base_lot_size: i64,
    pub quote_lot_size: i64,
    pub side: String,
    pub public_key: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub bind_ws_addr: String,
    pub source: SourceConfig,
    pub specs: Vec<Spec>
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub spec: Spec,
    pub orders: Vec<(i64, i64)>,
    pub slot: u64,
    pub write_version: u64
}

impl Serialize for Snapshot {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer
    {
        let mut state = serializer.serialize_struct("Snapshot", 3)?;

        state.serialize_field("market", &self.spec.market)?;
        state.serialize_field("type", "l2snapshot")?;
        state.serialize_field("side", &self.spec.side)?;
        state.serialize_field(
            "orders",
            &self.orders
                    .iter()
                    .map(|(price, quantity)| (
                        price.mul(
                            (10 as i64)
                                .pow((&self.spec.base_decimals - &self.spec.quote_decimals) as u32)
                                .mul(&self.spec.quote_lot_size)
                        ) as f64 / self.spec.base_lot_size as f64,
                        quantity.mul(
                            &self.spec.base_lot_size
                        ) as f64 / (10 as i64).pow(self.spec.base_decimals as u32) as f64
                    ))
                    .collect::<Vec<(f64, f64)>>()
        );
        state.serialize_field("slot", &self.slot)?;
        state.serialize_field("write_version", &self.write_version)?;

        state.end()
    }
}

#[derive(Clone, Debug)]
pub struct Delta {
    pub spec: Spec,
    pub orders: Vec<(i64, i64)>,
    pub slot: u64,
    pub write_version: u64
}

impl Serialize for Delta {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer
    {
        let mut state = serializer.serialize_struct("Snapshot", 3)?;

        state.serialize_field("market", &self.spec.market)?;
        state.serialize_field("type", "l2update")?;
        state.serialize_field("side", &self.spec.side)?;
        state.serialize_field(
            "orders",
            &self.orders
                .iter()
                .map(|(price, quantity)| (
                    price.mul(
                        (10 as i64)
                            .pow((&self.spec.base_decimals - &self.spec.quote_decimals) as u32)
                            .mul(&self.spec.quote_lot_size)
                    ) as f64 / self.spec.base_lot_size as f64,
                    quantity.mul(
                        &self.spec.base_lot_size
                    ) as f64 / (10 as i64).pow(self.spec.base_decimals as u32) as f64
                ))
                .collect::<Vec<(f64, f64)>>()
        );
        state.serialize_field("slot", &self.slot)?;
        state.serialize_field("write_version", &self.write_version)?;

        state.end()
    }
}


#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().collect();

    solana_logger::setup_with_default("info");

    if args.len() < 2 {
        eprintln!("Please enter a config file path argument.");
        return Ok(());
    }

    let config: Config = {
        let mut file = File::open(&args[1])?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        toml::from_str(&contents).unwrap()
    };

    let config_ref_thread = config.clone();

    let metrics_tx = metrics::start();

    info!("{:#?}", config);

    let (account_write_queue_sender, account_write_queue_receiver) =
        async_channel::unbounded::<AccountWrite>();

    let (slot_queue_sender, slot_queue_receiver) = async_channel::unbounded::<SlotUpdate>();

    let mut chain_cache = ChainData::new();

    let snapshots: Arc<Mutex<HashMap<String, Snapshot>>> = Arc::new(Mutex::new(HashMap::new()));

    let peers: Arc<Mutex<HashMap<SocketAddr, UnboundedSender<Message>>>> = Arc::new(Mutex::new(HashMap::new()));

    let snapshots_ref_thread = snapshots.clone();

    let peers_ref_thread = peers.clone();

    let mut trails = HashMap::<String, (u64, u64)>::new();

    let relevant_accounts = config.specs.clone().iter().map(|spec| spec.public_key.clone()).collect::<Vec<String>>();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                Ok(account_write) = account_write_queue_receiver.recv() => {
                    if !relevant_accounts.contains(&account_write.pubkey.to_string()) {
                        continue;
                    }

                    chain_cache.update_account(
                        account_write.pubkey,
                        AccountData {
                            slot: account_write.slot,
                            write_version: account_write.write_version,
                            account: WritableAccount::create(
                                account_write.lamports,
                                account_write.data.clone(),
                                account_write.owner,
                                account_write.executable,
                                account_write.rent_epoch as Epoch,
                            ),
                        },
                    );

                    info!(
                        "account write slot:{} pk:{:?} wv:{}",
                        account_write.slot,
                        account_write.pubkey,
                        account_write.write_version
                    );
                }
                Ok(slot_update) = slot_queue_receiver.recv() => {
                    chain_cache.update_slot(SlotData {
                        slot: slot_update.slot,
                        parent: slot_update.parent,
                        status: slot_update.status,
                        chain: 0,
                    });
                }
            };

            for spec in config_ref_thread.specs.iter() {
                let key = Pubkey::from_str(&spec.public_key).unwrap();

                let try_cache = chain_cache.account(&key);

                if try_cache.is_err() {
                    continue;
                }

                let cache = try_cache.unwrap();

                let meta = (cache.slot, cache.write_version);

                let trail = trails.get(&spec.market).unwrap_or(&(0, 0));

                if *trail == meta {
                    continue;
                }

                trails.insert(key.to_string().clone(), meta);

                let book_side: &BookSide = bytemuck::from_bytes(cache.account.data());

                let current_l2_snapshot: Vec<(i64, i64)> = book_side
                    .iter_valid(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs())
                    .map(|(node_handle, leaf_node)| (leaf_node.price(), leaf_node.quantity))
                    .group_by(|(price, quantity)| *price)
                    .into_iter()
                    .map(|(price, group)| (price, group.map(|(price, quantity)| quantity).fold(0, |acc, x| acc + x)))
                    .collect();

                let mut diff: Vec<(i64, i64)> = vec!();

                let snapshots_ref_thread_copy = snapshots_ref_thread.lock().unwrap().clone();

                if let Some(previous_l2_snapshot) = snapshots_ref_thread_copy.get(&key.to_string()) {
                    for previous_order in previous_l2_snapshot.orders.iter() {
                        let (previous_order_price, previous_order_size) = *previous_order;

                        let peer = current_l2_snapshot
                            .iter()
                            .find(|&(current_order_price, current_order_size)| previous_order_price == *current_order_price);

                        match peer {
                            None => diff.push((previous_order_price, 0)),
                            _ => continue
                        }
                    }

                    for current_order in &current_l2_snapshot {
                        let (current_order_price, current_order_size) = *current_order;

                        let peer = previous_l2_snapshot
                            .orders
                            .iter()
                            .find(|&(previous_order_price, previous_order_size)| *previous_order_price == current_order_price);

                        match peer {
                            Some(previous_order) => {
                                let (previous_order_price, previous_order_size) = previous_order;

                                if *previous_order_size == current_order_size {
                                    continue;
                                }

                                diff.push(current_order.clone());
                            },
                            None => diff.push(current_order.clone())
                        }
                    }
                }

                snapshots_ref_thread
                    .lock()
                    .unwrap()
                    .insert(
                        key.to_string(),
                        Snapshot {
                            spec: spec.clone(),
                            orders: current_l2_snapshot.clone(),
                            slot: cache.slot,
                            write_version: cache.write_version
                        }
                    );

                let mut ref_copy = peers_ref_thread.lock().unwrap().clone();

                if diff.len() == 0 {
                    continue
                }

                for (sock_addr, channel) in ref_copy.iter_mut() {
                    trace!("  > {}", sock_addr);

                    let delta = Delta {
                        spec: spec.clone(),
                        orders: diff.clone(),
                        slot: cache.slot,
                        write_version: cache.write_version
                    };

                    let json = serde_json::to_string(&delta);

                    let result = channel.send(Message::Text(json.unwrap())).await;

                    if result.is_err() {
                        error!("ws update error",)
                    }
                }
            }
        }
    });

    let try_socket = TcpListener::bind(&config.bind_ws_addr).await;

    let listener = try_socket.expect("failed to bind");

    info!("ws listening on: {}", &config.bind_ws_addr);

    tokio::spawn(async move {
        // Let's spawn the handling of each connection in a separate task.
        while let Ok((stream, addr)) = listener.accept().await {
            tokio::spawn(accept_connection_error(
                stream,
                addr,
                snapshots.clone(),
                peers.clone()
            ));
        }
    });


    grpc_plugin_source::process_events(
        &config.source,
        account_write_queue_sender,
        slot_queue_sender,
        metrics_tx,
    )
    .await;

    Ok(())
}

async fn accept_connection_error(
    stream: TcpStream,
    addr: SocketAddr,
    snapshots: Arc<Mutex<HashMap<String, Snapshot>>>,
    peers: Arc<Mutex<HashMap<SocketAddr, UnboundedSender<Message>>>>
) {
    let result = accept_connection(stream, addr, snapshots, peers.clone()).await;

    if result.is_err() {
        error!("connection {} error {}", addr, result.unwrap_err());
    };

    peers.lock().unwrap().remove(&addr);
}


async fn accept_connection(
    stream: TcpStream,
    addr: SocketAddr,
    snapshots: Arc<Mutex<HashMap<String, Snapshot>>>,
    peers: Arc<Mutex<HashMap<SocketAddr, UnboundedSender<Message>>>>
) -> Result<(), Error> {
    info!("new ws connection: {}", addr);

    let ws_stream = tokio_tungstenite::accept_async(stream)
        .await
        .expect("error during the ws handshake occurred");

    let (mut write, read) = ws_stream.split();

    let (sender, receiver) = unbounded();

    {
        peers.lock().unwrap().insert(addr, sender);
        info!("ws published: {}", addr);
    }

    let copy = snapshots.lock().unwrap().clone();

    for (_, snapshot) in copy.iter() {
        write
            .feed(Message::Text(serde_json::to_string(snapshot).unwrap()))
            .await?;
    }

    info!("ws snapshot sent: {}", addr);
    write.flush().await?;

    let forward_updates = receiver.map(Ok).forward(write);

    forward_updates.await?;

    info!("ws disconnected: {}", &addr);

    Ok( () )
}