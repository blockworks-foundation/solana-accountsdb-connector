use crate::{
    mango_v3_perp_crank_sink::MangoV3PerpCrankSink, mango_v4_perp_crank_sink::MangoV4PerpCrankSink,
    openbook_crank_sink::OpenbookCrankSink,
};
use solana_geyser_connector_lib::{
    account_write_filter::{self, AccountWriteRoute},
    metrics::Metrics,
    AccountWrite, SlotUpdate,
};
use solana_sdk::{instruction::Instruction, pubkey::Pubkey};
use std::{sync::Arc, time::Duration};

const TIMEOUT_INTERVAL: Duration = Duration::from_millis(400);

pub async fn init(
    v4_perp_market_pks: Vec<(Pubkey, Pubkey)>,
    v3_perp_market_pks: Vec<(Pubkey, Pubkey)>,
    openbook_pks: Vec<(Pubkey, Pubkey)>,
    v4_group_pk: Pubkey,
    v3_group_pks: (Pubkey, Pubkey, Pubkey),
    metrics_sender: Metrics,
) -> anyhow::Result<(
    async_channel::Sender<AccountWrite>,
    async_channel::Sender<SlotUpdate>,
    async_channel::Receiver<Vec<Instruction>>,
)> {
    // Event queue updates can be consumed by client connections
    let (instruction_sender, instruction_receiver) = async_channel::unbounded::<Vec<Instruction>>();

    let routes = vec![
        AccountWriteRoute {
            matched_pubkeys: openbook_pks
                .iter()
                .map(|(_, evq_pk)| evq_pk.clone())
                .collect(),
            sink: Arc::new(OpenbookCrankSink::new(
                openbook_pks,
                instruction_sender.clone(),
            )),
            timeout_interval: TIMEOUT_INTERVAL,
        },
        AccountWriteRoute {
            matched_pubkeys: v3_perp_market_pks
                .iter()
                .map(|(_, evq_pk)| evq_pk.clone())
                .collect(),
            sink: Arc::new(MangoV3PerpCrankSink::new(
                v3_perp_market_pks,
                v3_group_pks.0,
                v3_group_pks.1,
                v3_group_pks.2,
                instruction_sender.clone(),
            )),
            timeout_interval: Duration::default(),
        },
        AccountWriteRoute {
            matched_pubkeys: v4_perp_market_pks
                .iter()
                .map(|(_, evq_pk)| evq_pk.clone())
                .collect(),
            sink: Arc::new(MangoV4PerpCrankSink::new(
                v4_perp_market_pks,
                v4_group_pk,
                instruction_sender.clone(),
            )),
            timeout_interval: Duration::default(),
        },
    ];

    let (account_write_queue_sender, slot_queue_sender) =
        account_write_filter::init(routes, metrics_sender.clone())?;

    Ok((
        account_write_queue_sender,
        slot_queue_sender,
        instruction_receiver,
    ))
}
