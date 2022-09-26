use {
    log::*,
    serde::Deserialize,
    solana_geyser_connector_lib::{chain_data::*, *},
    solana_sdk::{account::WritableAccount, clock::Epoch, pubkey::Pubkey},
    std::{fs::File, io::Read, str::FromStr},
};

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub source: SourceConfig,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        println!("requires a config file argument");
        return Ok(());
    }

    let config: Config = {
        let mut file = File::open(&args[1])?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        toml::from_str(&contents).unwrap()
    };

    solana_logger::setup_with_default("info");
    info!("startup");

    let metrics_tx = metrics::start();

    let (account_write_queue_sender, account_write_queue_receiver) =
        async_channel::unbounded::<AccountWrite>();
    let (slot_queue_sender, slot_queue_receiver) = async_channel::unbounded::<SlotUpdate>();
    let mut chain_cache = ChainData::new();
    // SOL-PERP event q
    let events_pk = Pubkey::from_str("31cKs646dt1YkA3zPyxZ7rUAkxTBz279w4XEobFXcAKP").unwrap();
    let mut last_ev_q_version = (0u64, 0u64);

    tokio::spawn(async move {
        loop {
            tokio::select! {
                Ok(account_write) = account_write_queue_receiver.recv() => {
                    trace!("account write slot:{} pk:{:?} wv:{}", account_write.slot, account_write.pubkey, account_write.write_version);

                    if account_write.pubkey == events_pk {
                        info!("account write slot:{} pk:{:?} wv:{}", account_write.slot, account_write.pubkey, account_write.write_version);

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

            let events_acc = chain_cache.account(&events_pk);
            if events_acc.is_ok() {
                let acc = events_acc.unwrap();

                let ev_q_version = (acc.slot, acc.write_version);
                if ev_q_version != last_ev_q_version {
                    info!("chain cache slot:{} wv:{}", acc.slot, acc.write_version);
                    last_ev_q_version = ev_q_version;
                }
            }
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
