use mangol_common::errors::MangolResult;
use mangol_solana::connection::SolanaConnection;
use solana_program::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::transaction::Transaction;
use std::str::FromStr;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use solana_program::clock::UnixTimestamp;
use solana_sdk::commitment_config::CommitmentConfig;
use crate::types::{OrderType, PerpMarketData, Side, MangoGroup, MangoCache, MangoAccount, ExpiryType, PerpMarketInfo};
use solana_sdk::signature::Signer;
use crate::types::PerpMarket;
pub struct MangoClient {
	pub solana_connection: SolanaConnection,
	pub mango_account: MangoAccount,
	pub mango_account_pk: Pubkey,
	pub mango_cache: MangoCache,
	pub mango_cache_pk: Pubkey,
	pub mango_group: MangoGroup,
	pub mango_group_pk: Pubkey,
	pub mango_program_id: Pubkey,
	pub signer: Keypair
}

impl MangoClient {
	pub fn new(solana_connection: &SolanaConnection, mango_group: MangoGroup, mango_group_pk: Pubkey, mango_account_pk: Pubkey, mango_cache_pk: Pubkey, mango_account: MangoAccount, mango_cache: MangoCache, program_id: Pubkey, signer: Keypair) ->
	MangolResult<Self> {
		let my_connection = SolanaConnection::new(&solana_connection.rpc_client.url())?;
		Ok(Self {
			solana_connection: my_connection,
			mango_account,
			mango_cache,
			mango_group,
			mango_group_pk,
			mango_account_pk,
			mango_cache_pk,
			mango_program_id: program_id,
			signer
		})
	}
	
	pub fn update(&mut self) -> MangolResult<()> {
		let mango_account_info = self.solana_connection.rpc_client.get_account_with_commitment(&self.mango_account_pk, CommitmentConfig::finalized()).unwrap().value.unwrap();
		self.mango_account = MangoAccount::load_checked(mango_account_info, &self.mango_program_id).unwrap();
		
		let mango_group_account_info = self.solana_connection.rpc_client.get_account_with_commitment(&self.mango_group_pk, CommitmentConfig::finalized()).unwrap().value.unwrap();
		self.mango_group = MangoGroup::load_checked(mango_group_account_info, &self.mango_program_id).unwrap();
		let mango_cache_account_info = self.solana_connection.rpc_client.get_account_with_commitment(&self.mango_group.mango_cache, CommitmentConfig::finalized())?.value.unwrap();
		self.mango_cache = MangoCache::load_checked(mango_cache_account_info, &self.mango_program_id, &self.mango_group).unwrap();
		Ok(())
	}
	
	pub fn place_perp_order(&self, perp_market: &PerpMarketInfo, perp_market_data: &PerpMarketData, side: Side, price: f64, quantity: i64, order_type: OrderType, reduce_only: bool, expiry_timestamp: Option<u64>) -> MangolResult<String> {
		let (native_price, native_quantity) = perp_market.lotToNativePriceQuantity(price, quantity.try_into().unwrap());
		println!("Order price: {} Order quantity: {}", price * 100, quantity * 100 / native_price);
		let mut expires_at = None;
		if expiry_timestamp.is_some() {
			expires_at = Some(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() + expiry_timestamp.unwrap());
		}
		let instruction = crate::instructions::place_perp_order2(
			&self.mango_program_id,
			&self.mango_group_pk,
			&self.mango_account_pk,
			&self.mango_account.owner,
			&self.mango_cache_pk,
			&Pubkey::from_str(&perp_market_data.pubkey).unwrap(),
			&Pubkey::from_str(&perp_market_data.bids_key.clone()).unwrap(),
			&Pubkey::from_str(&perp_market_data.asks_key.clone()).unwrap(),
			&Pubkey::from_str(&perp_market_data.events_key.clone()).unwrap(),
			None,
			&self.mango_account.spot_open_orders,
			side,
			native_price,
			quantity / native_price,
			quantity,
			0,
			order_type,
			reduce_only,
			expires_at,
			10,
			ExpiryType::Absolute).unwrap();
		let mut mango_accounts_to_consume_events = [self.mango_account_pk.clone()];
		let consume_instruction = crate::instructions::consume_events(
			&self.mango_program_id,
			&self.mango_group_pk,
			&self.mango_group.mango_cache,
			&Pubkey::from_str(&perp_market_data.pubkey).unwrap(),
			&Pubkey::from_str(&perp_market_data.events_key.clone()).unwrap(),
			&mut mango_accounts_to_consume_events,
			4
		).unwrap();
		let mut transaction = Transaction::new_with_payer(&[instruction, consume_instruction], Some(&self.signer.pubkey()));
		self.solana_connection.try_tx_once(transaction, &self.signer)
		
	}
	

	
	pub fn place_perp_order_with_base(&self, perp_market: &PerpMarketInfo, perp_market_data: &PerpMarketData, side: Side, price: f64, quantity: i64, order_type: OrderType, reduce_only: bool, expiry_timestamp: Option<u64>) -> MangolResult<String> {
		let (native_price, native_quantity) = perp_market.lotToNativePriceQuantity(price, quantity.try_into().unwrap());
		let mut expires_at = None;
		if (expiry_timestamp.is_some()) {
			expires_at = Some(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() + expiry_timestamp.unwrap());
		}
		let instruction = crate::instructions::place_perp_order2(
			&self.mango_program_id,
			&self.mango_group_pk,
			&self.mango_account_pk,
			&self.mango_account.owner,
			&self.mango_cache_pk,
			&Pubkey::from_str(&perp_market_data.pubkey).unwrap(),
			&Pubkey::from_str(&perp_market_data.bids_key.clone()).unwrap(),
			&Pubkey::from_str(&perp_market_data.asks_key.clone()).unwrap(),
			&Pubkey::from_str(&perp_market_data.events_key.clone()).unwrap(),
			None,
			&self.mango_account.spot_open_orders,
			side,
			native_price,
			quantity,
			quantity * native_price,
			0,
			order_type,
			reduce_only,
			expires_at,
			10,
			ExpiryType::Absolute).unwrap();
		let mut mango_accounts_to_consume_events = [self.mango_account_pk.clone()];
		
		let consume_instruction = crate::instructions::consume_events(
			&self.mango_program_id,
			&self.mango_group_pk,
			&self.mango_group.mango_cache,
			&Pubkey::from_str(&perp_market_data.pubkey).unwrap(),
			&Pubkey::from_str(&perp_market_data.events_key.clone()).unwrap(),
			&mut mango_accounts_to_consume_events,
			4
		).unwrap();
		let mut transaction = Transaction::new_with_payer(&[instruction, consume_instruction], Some(&self.signer.pubkey()));
		self.solana_connection.try_tx_once(transaction, &self.signer)
		
	}
	

	

}