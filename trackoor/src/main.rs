use std::any::Any;
use std::ops::Div;
use std::time::{SystemTime, UNIX_EPOCH};

use arrayref::array_ref;
use itertools::Itertools;
use mango::matching::{AnyNode, BookSide, LeafNode, NodeHandle};
use mango::queue::{AnyEvent, EventQueueHeader, EventType, FillEvent};

use {
    log::*,
    serde::Deserialize,
    solana_geyser_connector_lib::{*, chain_data::*},
    solana_sdk::{account::{ReadableAccount, WritableAccount}, clock::Epoch, pubkey::Pubkey},
    std::{fs::File, io::Read, mem::size_of, str::FromStr},
};

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub source: SourceConfig,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

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

    let metrics_tx = metrics::start();

    let (account_write_queue_sender, account_write_queue_receiver) =
        async_channel::unbounded::<AccountWrite>();

    let (slot_queue_sender, slot_queue_receiver) = async_channel::unbounded::<SlotUpdate>();

    let mut chain_cache = ChainData::new();

    let events_pk = Pubkey::from_str("Fu8q5EiFunGwSRrjFKjRUoMABj5yCoMEPccMbUiAT6PD").unwrap();

    tokio::spawn(async move {
        let mut trailing_l2_snapshot: Option<Vec<(i64, i64)>> = None;

        loop {
            tokio::select! {
                Ok(account_write) = account_write_queue_receiver.recv() => {
                    if account_write.pubkey != events_pk {
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

                    info!("account write slot:{} pk:{:?} wv:{}", account_write.slot, account_write.pubkey, account_write.write_version);

                    let cache = chain_cache.account(&events_pk);

                    if cache.is_err() {
                        continue;
                    }

                    let book_side: &BookSide = bytemuck::from_bytes(cache.unwrap().account.data());

                    let current_l2_snapshot: Vec<(i64, i64)> = book_side
                        .iter_valid(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs())
                        .map(|(node_handle, leaf_node)| (leaf_node.price(), leaf_node.quantity))
                        .group_by(|(price, quantity)| *price)
                        .into_iter()
                        .map(|(price, group)| (price, group.map(|(price, quantity)| quantity).fold(0, |acc, x| acc + x)))
                        .collect();

                    // TODO: Figure out how to run the diff only at the end of each block instead of at every write

                    info!("{:?}", &current_l2_snapshot[0..3]);

                    let mut diff: Vec<(i64, i64)> = vec!();

                    if let Some(ref previous_l2_snapshot) = trailing_l2_snapshot {
                        println!("Previous snapshot - diff and send deltas.");

                        for previous_order in previous_l2_snapshot {
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
                    } else {
                        println!("No previous snapshot - just send over the current one.");

                        for current_order in &current_l2_snapshot {
                            diff.push(current_order.clone());
                        }
                    }

                    let base_decimals = 9 as f64;

                    let quote_decimals = 6 as f64;

                    let quote_lot_size = 100 as f64;

                    let base_lot_size = 10000000 as f64;

                    let price_lots_to_ui_convertor = (10 as f64)
                        .powf(base_decimals - quote_decimals)
                        * quote_lot_size
                        / base_lot_size;

                    let base_lots_to_ui_convertor = base_lot_size / (10 as f64).powf(base_decimals);

                    // let price_ui = leaf_node.price() as f64 * price_lots_to_ui_convertor;
                    //
                    // let quantity_ui = leaf_node.quantity as f64 * base_lots_to_ui_convertor;

                    println!(
                        "{:#?}",
                        diff.iter().map(|(price, quantity)| (*price as f64 * price_lots_to_ui_convertor, *quantity as f64 * base_lots_to_ui_convertor)).collect::<Vec<(f64, f64)>>()
                    );

                    trailing_l2_snapshot = Some(current_l2_snapshot);
                }
                Ok(slot_update) = slot_queue_receiver.recv() => {
                    chain_cache.update_slot(SlotData {
                        slot: slot_update.slot,
                        parent: slot_update.parent,
                        status: slot_update.status,
                        chain: 0,
                    });

                    info!("slot update {} {:?} {:?}", slot_update.slot, slot_update.parent, slot_update.status);
                }
            };
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
