use jormungandr_lib::interfaces::*;
use jormungandr_lib::time::SystemTime;

use actix_web::error::{Error, ErrorBadRequest, ErrorInternalServerError, ErrorNotFound};
use actix_web::{Error as ActixError, HttpMessage, HttpRequest, HttpResponse};
use actix_web::{Json, Path, Query, Responder, State};
use chain_core::property::Deserialize;
use chain_crypto::{Blake2b256, PublicKey};
use chain_impl_mockchain::account::{AccountAlg, Identifier};
use chain_impl_mockchain::fragment::Fragment;
use chain_impl_mockchain::key::Hash;
use chain_impl_mockchain::leadership::{Leader, LeadershipConsensus};
use chain_impl_mockchain::value::{Value, ValueError};

use crate::intercom::TransactionMsg;
use crate::secure::NodeSecret;
use bytes::{Bytes, IntoBuf};
use futures::Future;
use std::str::FromStr;

pub use crate::rest::Context;

pub fn get_utxos(context: State<Context>) -> impl Responder {
    let tip_reference = context.blockchain_tip.get_ref().wait().unwrap();
    let utxos = tip_reference.ledger().utxos();
    let utxos = utxos.map(UTxOInfo::from).collect::<Vec<_>>();
    Json(utxos)
}

pub fn get_account_state(
    context: State<Context>,
    account_id_hex: Path<String>,
) -> Result<impl Responder, Error> {
    let account_id = parse_account_id(&account_id_hex)?;

    let tip_reference = context.blockchain_tip.get_ref().wait().unwrap();
    let state = tip_reference
        .ledger()
        .accounts()
        .get_state(&account_id)
        .map_err(|e| ErrorNotFound(e))?;

    Ok(Json(AccountState::from(state)))
}

fn parse_account_id(id_hex: &str) -> Result<Identifier, Error> {
    PublicKey::<AccountAlg>::from_str(id_hex)
        .map(Into::into)
        .map_err(|e| ErrorBadRequest(e))
}

pub fn get_message_logs(context: State<Context>) -> impl Responder {
    let logs = context.logs.lock().unwrap();
    let logs = logs.logs().wait().unwrap();
    Json(logs)
}

pub fn post_message(
    request: &HttpRequest<Context>,
) -> impl Future<Item = impl Responder + 'static, Error = impl Into<ActixError> + 'static> + 'static
{
    let sender = request.state().transaction_task.clone();
    request.body().map(move |message| -> Result<_, ActixError> {
        let msg = Fragment::deserialize(message.into_buf()).map_err(|e| {
            println!("{}", e);
            ErrorBadRequest(e)
        })?;
        let msg = TransactionMsg::SendTransaction(FragmentOrigin::Rest, vec![msg]);
        sender.lock().unwrap().try_send(msg).unwrap();
        Ok("")
    })
}

pub fn get_tip(settings: State<Context>) -> impl Responder {
    settings
        .blockchain_tip
        .get_ref()
        .wait()
        .unwrap()
        .hash()
        .to_string()
}

pub fn get_stats_counter(context: State<Context>) -> Result<impl Responder, Error> {
    let mut block_tx_count = 0;
    let mut block_input_sum = Value::zero();
    let mut block_fee_sum = Value::zero();

    let tip = context.blockchain_tip.get_ref().wait().unwrap();
    let storage = context.blockchain.storage();
    // let block_tip = storage.get(tip.hash().clone()).wait().unwrap().unwrap();
    /*
        block_tip
            .contents
            .iter()
            .filter_map(|fragment| match fragment {
                Fragment::Transaction(tx) => Some(&tx.transaction),
                _ => None,
            })
            .map(|tx| {
                let input_sum = Value::sum(tx.inputs.iter().map(|input| input.value))?;
                let output_sum = Value::sum(tx.outputs.iter().map(|input| input.value))?;
                // Input < output implies minting, so no fee
                let fee = (input_sum - output_sum).unwrap_or(Value::zero());
                block_tx_count += 1;
                block_input_sum = (block_input_sum + input_sum)?;
                block_fee_sum = (block_fee_sum + fee)?;
                Ok(())
            })
            .collect::<Result<(), ValueError>>()
            .map_err(|e| ErrorInternalServerError(format!("Block value calculation error: {}", e)))?;
    */
    let stats = &context.stats_counter;
    Ok(Json(json!({
        "txRecvCnt": stats.tx_recv_cnt(),
        "blockRecvCnt": stats.block_recv_cnt(),
        "uptime": stats.uptime_sec(),
        "lastBlockTime": stats.slot_start_time().map(SystemTime::from),
        "lastBlockTx": block_tx_count,
        "lastBlockSum": block_input_sum.0,
        "lastBlockFees": block_fee_sum.0,
    })))
}

pub fn get_block_id(
    context: State<Context>,
    block_id_hex: Path<String>,
) -> Result<Bytes, ActixError> {
    use chain_core::property::Serialize as _;

    let block_id = parse_block_hash(&block_id_hex)?;

    let storage = context.blockchain.storage();
    let block = storage.get(block_id).wait().unwrap().unwrap();
    let block = block.serialize_as_vec().unwrap();

    Ok(Bytes::from(block))
}

fn parse_block_hash(hex: &str) -> Result<Hash, ActixError> {
    let hash: Blake2b256 = hex.parse().map_err(|e| ErrorBadRequest(e))?;
    Ok(Hash::from(hash))
}

pub fn get_block_next_id(
    context: State<Context>,
    block_id_hex: Path<String>,
    query_params: Query<QueryParams>,
) -> Result<Bytes, ActixError> {
    use chain_storage::store;

    let block_id = parse_block_hash(&block_id_hex)?;

    // FIXME
    // POSSIBLE RACE CONDITION OR DEADLOCK!
    // Assuming that during update whole blockchain is write-locked
    // FIXME: don't hog the blockchain lock.
    let storage = context.blockchain.storage().get_inner().wait().unwrap();
    let tip = context.blockchain_tip.get_ref().wait().unwrap();
    store::iterate_range(&*storage, &block_id, tip.hash())
        .map_err(|e| ErrorBadRequest(e))?
        .take(query_params.get_count())
        .try_fold(Bytes::new(), |mut bytes, res| {
            let block_info = res.map_err(|e| ErrorInternalServerError(e))?;
            bytes.extend_from_slice(block_info.block_hash.as_ref());
            Ok(bytes)
        })
}

const MAX_COUNT: usize = 100;

#[derive(Deserialize)]
pub struct QueryParams {
    count: Option<usize>,
}

impl QueryParams {
    pub fn get_count(&self) -> usize {
        self.count.unwrap_or(1).min(MAX_COUNT)
    }
}

pub fn get_stake_distribution(context: State<Context>) -> Result<impl Responder, Error> {
    let blockchain_tip = context.blockchain_tip.get_ref().wait().unwrap();

    let leadership = blockchain_tip.epoch_leadership_schedule();
    let last_epoch = blockchain_tip.block_date().epoch;
    if let LeadershipConsensus::GenesisPraos(gp) = leadership.consensus() {
        let stake = gp.distribution();
        let pools: Vec<_> = stake
            .to_pools
            .iter()
            .map(|(h, p)| (format!("{}", h), p.total_stake.0))
            .collect();
        Ok(Json(json!({
            "epoch": last_epoch,
            "stake": {
                "unassigned": stake.unassigned.0,
                "dangling": stake.dangling.0,
                "pools": pools,
            }
        })))
    } else {
        Ok(Json(json!({ "epoch": last_epoch })))
    }
}

pub fn get_settings(context: State<Context>) -> Result<impl Responder, Error> {
    let blockchain_tip = context.blockchain_tip.get_ref().wait().unwrap();

    let ledger = blockchain_tip.ledger();
    let static_params = ledger.get_static_parameters();
    let consensus_version = ledger.consensus_version();
    let current_params = blockchain_tip.epoch_ledger_parameters();
    let fees = current_params.fees;

    Ok(Json(json!({
        "block0Hash": static_params.block0_initial_hash.to_string(),
        "block0Time": SystemTime::from_secs_since_epoch(static_params.block0_start_time.0),
        "currSlotStartTime": context.stats_counter.slot_start_time().map(SystemTime::from),
        "consensusVersion": consensus_version.to_string(),
        "fees":{
            "constant": fees.constant,
            "coefficient": fees.coefficient,
            "certificate": fees.certificate,
        },
        "maxTxsPerBlock": 255, // TODO?
    })))
}

pub fn get_shutdown(context: State<Context>) -> Result<impl Responder, Error> {
    // Server finishes ongoing tasks before stopping, so user will get response to this request
    // Node should be shutdown automatically when server stopping is finished
    context
        .server
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .ok_or_else(|| ErrorInternalServerError("Server not set in context"))?
        .stop();
    Ok(HttpResponse::Ok().finish())
}

pub fn get_leaders(context: State<Context>) -> impl Responder {
    Json(json! {
        context.enclave.get_leaderids()
    })
}

pub fn post_leaders(secret: Json<NodeSecret>, context: State<Context>) -> impl Responder {
    let leader = Leader {
        bft_leader: secret.bft(),
        genesis_leader: secret.genesis(),
    };
    let leader_id = context.enclave.add_leader(leader);
    Json(leader_id)
}

pub fn delete_leaders(
    context: State<Context>,
    leader_id: Path<EnclaveLeaderId>,
) -> Result<impl Responder, Error> {
    match context.enclave.remove_leader(*leader_id) {
        true => Ok(HttpResponse::Ok().finish()),
        false => Err(ErrorNotFound("Leader with given ID not found")),
    }
}

pub fn get_stake_pools(context: State<Context>) -> Result<impl Responder, Error> {
    let blockchain_tip = context.blockchain_tip.get_ref().wait().unwrap();

    let stake_pool_ids = blockchain_tip
        .ledger()
        .delegation()
        .stake_pool_ids()
        .map(|id| id.to_string())
        .collect::<Vec<_>>();
    Ok(Json(stake_pool_ids))
}