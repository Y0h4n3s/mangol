use std::cell::{Ref, RefMut};
use std::cmp::{max, min};
use std::convert::{identity, TryFrom};
use std::mem::size_of;
use std::ops::Deref;

use bytemuck::{cast_ref, from_bytes, from_bytes_mut, try_from_bytes_mut};
use enumflags2::BitFlags;
use fixed::types::I80F48;
use fixed_macro::types::I80F48;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use serde::{Deserialize, Serialize};
use serum_dex::state::{OpenOrders, ToAlignedBytes};
use solana_sdk::account::Account as AccountInfo;
use solana_program::program_error::ProgramError;
use solana_program::program_pack::Pack;
use solana_program::pubkey::Pubkey;
use solana_program::sysvar::{clock::Clock, rent::Rent, Sysvar};
use spl_token::state::Account;
use static_assertions::const_assert_eq;

use mangol_common::Loadable;
use mango_macro::{Loadable, Pod, TriviallyTransmutable};

use crate::error::{check_assert, MangoError, MangoErrorCode, MangoResult, SourceFileId};
use crate::ids::mngo_token;
use crate::utils::{
	compute_interest_rate, invert_side, pow_i80f48, remove_slop_mut, split_open_orders,
};

pub const MAX_TOKENS: usize = 16; // Just changed
pub const MAX_PAIRS: usize = MAX_TOKENS - 1;
pub const MAX_NODE_BANKS: usize = 8;
pub const QUOTE_INDEX: usize = MAX_TOKENS - 1;
pub const ZERO_I80F48: I80F48 = I80F48!(0);
pub const ONE_I80F48: I80F48 = I80F48!(1);
pub const NEG_ONE_I80F48: I80F48 = I80F48!(-1);
pub const DAY: I80F48 = I80F48!(86400);
pub const YEAR: I80F48 = I80F48!(31536000);

pub const DUST_THRESHOLD: I80F48 = I80F48!(0.000001); // TODO make this part of MangoGroup state
const MAX_RATE_ADJ: I80F48 = I80F48!(4); // TODO make this part of PerpMarket if we want per market flexibility
const MIN_RATE_ADJ: I80F48 = I80F48!(0.25);
pub const INFO_LEN: usize = 32;
pub const MAX_PERP_OPEN_ORDERS: usize = 64;
pub const FREE_ORDER_SLOT: u8 = u8::MAX;
pub const MAX_NUM_IN_MARGIN_BASKET: u8 = 9;
pub const INDEX_START: I80F48 = I80F48!(1_000_000);
pub const PYTH_CONF_FILTER: I80F48 = I80F48!(0.10); // filter out pyth prices with conf > 10% of price
pub const CENTIBPS_PER_UNIT: I80F48 = I80F48!(1_000_000);


// NOTE: I80F48 multiplication ops are very expensive. Avoid when possible
// TODO: add prop tests for nums
// TODO add GUI hoster fee discount

// units
// long_funding: I80F48 - native quote currency per contract
// short_funding: I80F48 - native quote currency per contract
// long_funding_settled: I80F48 - native quote currency per contract
// short_funding_settled: I80F48 - native quote currency per contract
// base_positions: i64 - number of contracts
// quote_positions: I80F48 - native quote currency
// price: I80F48 - native quote per native base
// price: i64 - quote lots per base lot
//
#[derive(
Eq, PartialEq, Copy, Clone, TryFromPrimitive, IntoPrimitive, Debug, Serialize, Deserialize,
)]
#[repr(u8)]
#[serde(into = "u8", try_from = "u8")]
pub enum ExpiryType {
	/// Expire at exactly the given block time.
	///
	/// Orders with an expiry in the past are ignored. Expiry more than 255s in the future
	/// is clamped to 255 seconds.
	Absolute,
	
	/// Expire a number of block time seconds in the future.
	///
	/// Must be between 1 and 255.
	Relative,
}


#[derive(
Eq, PartialEq, Copy, Clone, TryFromPrimitive, IntoPrimitive, Debug, Serialize, Deserialize,
)]
#[repr(u8)]
#[serde(into = "u8", try_from = "u8")]
pub enum OrderType {
	/// Take existing orders up to price, max_base_quantity and max_quote_quantity.
	/// If any base_quantity or quote_quantity remains, place an order on the book
	Limit = 0,
	
	/// Take existing orders up to price, max_base_quantity and max_quote_quantity.
	/// Never place an order on the book.
	ImmediateOrCancel = 1,
	
	/// Never take any existing orders, post the order on the book if possible.
	/// If existing orders can match with this order, do nothing.
	PostOnly = 2,
	
	/// Ignore price and take orders up to max_base_quantity and max_quote_quantity.
	/// Never place an order on the book.
	///
	/// Equivalent to ImmediateOrCancel with price=i64::MAX.
	Market = 3,
	
	/// If existing orders match with this order, adjust the price to just barely
	/// not match. Always places an order on the book.
	PostOnlySlide = 4,
}

#[derive(
Eq, PartialEq, Copy, Clone, TryFromPrimitive, IntoPrimitive, Debug, Serialize, Deserialize,
)]
#[repr(u8)]
#[serde(into = "u8", try_from = "u8")]
pub enum Side {
	Bid = 0,
	Ask = 1,
}


#[repr(u8)]
#[derive(IntoPrimitive, TryFromPrimitive)]
pub enum DataType {
	MangoGroup = 0,
	MangoAccount,
	RootBank,
	NodeBank,
	PerpMarket,
	Bids,
	Asks,
	MangoCache,
	AdvancedOrders,
	ReferrerMemory,
	ReferrerIdRecord,
}

const NUM_HEALTHS: usize = 3;
#[repr(usize)]
#[derive(Copy, Clone, IntoPrimitive, TryFromPrimitive)]
pub enum HealthType {
	/// Maintenance health. If this health falls below 0 you get liquidated
	Maint,
	
	/// Initial health. If this falls below 0 you cannot open more positions
	Init,
	
	/// This is just the account equity i.e. unweighted sum of value of assets minus liabilities
	Equity,
}

#[derive(
Eq, PartialEq, Copy, Clone, TryFromPrimitive, IntoPrimitive, Serialize, Deserialize, Debug,
)]
#[repr(u8)]
#[serde(into = "u8", try_from = "u8")]
pub enum AssetType {
	Token = 0,
	Perp = 1,
}

#[derive(Copy, Clone, Debug,Pod, Default)]
#[repr(C)]
/// Stores meta information about the `Account` on chain
pub struct MetaData {
	pub data_type: u8,
	pub version: u8,
	pub is_initialized: bool,
	// being used by PerpMarket to store liquidity mining param
	pub extra_info: [u8; 5],
}

impl MetaData {
	pub fn new(data_type: DataType, version: u8, is_initialized: bool) -> Self {
		Self { data_type: data_type as u8, version, is_initialized, extra_info: [0; 5] }
	}
	pub fn new_with_extra(
		data_type: DataType,
		version: u8,
		is_initialized: bool,
		extra_info: [u8; 5],
	) -> Self {
		Self { data_type: data_type as u8, version, is_initialized, extra_info }
	}
}

#[derive(Copy, Clone, Pod)]
#[repr(C)]
pub struct TokenInfo {
	pub mint: Pubkey,
	pub root_bank: Pubkey,
	pub decimals: u8,
	pub padding: [u8; 7],
}

impl TokenInfo {
	pub fn is_empty(&self) -> bool {
		self.mint == Pubkey::default()
	}
}

#[derive(Copy, Clone, Pod)]
#[repr(C)]
pub struct SpotMarketInfo {
	pub spot_market: Pubkey,
	pub maint_asset_weight: I80F48,
	pub init_asset_weight: I80F48,
	pub maint_liab_weight: I80F48,
	pub init_liab_weight: I80F48,
	pub liquidation_fee: I80F48,
}

impl SpotMarketInfo {
	pub fn is_empty(&self) -> bool {
		self.spot_market == Pubkey::default()
	}
}

#[derive(Copy, Clone, Pod)]
#[repr(C)]
pub struct PerpMarketInfo {
	pub perp_market: Pubkey, // One of these may be empty
	pub maint_asset_weight: I80F48,
	pub init_asset_weight: I80F48,
	pub maint_liab_weight: I80F48,
	pub init_liab_weight: I80F48,
	pub liquidation_fee: I80F48,
	pub maker_fee: I80F48,
	pub taker_fee: I80F48,
	pub base_lot_size: i64,  // The lot size of the underlying
	pub quote_lot_size: i64, // min tick
}

impl PerpMarketInfo {
	pub fn is_empty(&self) -> bool {
		self.perp_market == Pubkey::default()
	}
	// pub fn lot_to_native_price(&self, price: i64) -> I80F48 {
	// 	I80F48::from_num(price)
	// 		  .checked_mul(I80F48::from_num(self.quote_lot_size))
	// 		  .unwrap()
	// 		  .checked_div(I80F48::from_num(self.base_lot_size))
	// 		  .unwrap()
	// }
	
	pub fn lotToNativePriceQuantity(&self, price: f64, quantity: u64) -> (i64, i64) {
		let nativePrice = (price * self.base_lot_size as f64) / self.quote_lot_size as f64;
		let nativeQuantity = quantity as f64 / self.base_lot_size as f64;
		return (nativePrice.round().to_string().parse::<i64>().unwrap(), nativeQuantity.round().to_string().parse::<i64>().unwrap());
	}
	
	pub fn lot_to_native_price(&self, price: f64) -> i64 {
		((price * self.base_lot_size as f64) / self.quote_lot_size as f64).round().to_string().parse::<i64>().unwrap()
	}
	
	
}

#[derive(Copy, Clone, Pod, Loadable)]
#[repr(C)]
pub struct MangoGroup {
	pub meta_data: MetaData,
	pub num_oracles: usize, // incremented every time add_oracle is called
	
	pub tokens: [TokenInfo; MAX_TOKENS],
	pub spot_markets: [SpotMarketInfo; MAX_PAIRS],
	pub perp_markets: [PerpMarketInfo; MAX_PAIRS],
	
	pub oracles: [Pubkey; MAX_PAIRS],
	
	pub signer_nonce: u64,
	pub signer_key: Pubkey,
	pub admin: Pubkey,          // Used to add new markets and adjust risk params
	pub dex_program_id: Pubkey, // Consider allowing more
	pub mango_cache: Pubkey,
	pub valid_interval: u64,
	
	// insurance vault is funded by the Mango DAO with USDC and can be withdrawn by the DAO
	pub insurance_vault: Pubkey,
	pub srm_vault: Pubkey,
	pub msrm_vault: Pubkey,
	pub fees_vault: Pubkey,
	
	pub max_mango_accounts: u32, // limits maximum number of MangoAccounts.v1 (closeable) accounts
	pub num_mango_accounts: u32, // number of MangoAccounts.v1
	
	pub ref_surcharge_centibps: u32, // 100
	pub ref_share_centibps: u32,     // 8Buying0 (must be less than surcharge)
	pub ref_mngo_required: u64,
	pub padding: [u8; 8], // padding used for future expansions
}

impl MangoGroup {
	pub fn load_mut_checked(
		account: AccountInfo,
		program_id: &Pubkey,
	) -> MangoResult<Self> {
		
		let mango_group: &Self = Self::load_from_bytes(&account.data)?;
		
		Ok(mango_group.clone())
	}
	pub fn load_checked(
		account: AccountInfo,
		program_id: &Pubkey,
	) -> MangoResult<Self> {
		
		let mango_group: &Self = Self::load_from_bytes(&account.data)?;
		
		Ok(mango_group.clone())
	}
	
	pub fn find_oracle_index(&self, oracle_pk: &Pubkey) -> Option<usize> {
		self.oracles.iter().position(|pk| pk == oracle_pk) // TODO OPT profile
	}
	pub fn find_root_bank_index(&self, root_bank_pk: &Pubkey) -> Option<usize> {
		// TODO profile and optimize
		self.tokens.iter().position(|token_info| &token_info.root_bank == root_bank_pk)
	}
	pub fn find_token_index(&self, mint_pk: &Pubkey) -> Option<usize> {
		self.tokens.iter().position(|token_info| &token_info.mint == mint_pk)
	}
	pub fn find_spot_market_index(&self, spot_market_pk: &Pubkey) -> Option<usize> {
		self.spot_markets
		    .iter()
		    .position(|spot_market_info| &spot_market_info.spot_market == spot_market_pk)
	}
	pub fn find_perp_market_index(&self, perp_market_pk: &Pubkey) -> Option<usize> {
		self.perp_markets
		    .iter()
		    .position(|perp_market_info| &perp_market_info.perp_market == perp_market_pk)
	}
	pub fn get_token_asset_weight(&self, token_index: usize, health_type: HealthType) -> I80F48 {
		if token_index == QUOTE_INDEX {
			ONE_I80F48
		} else {
			match health_type {
				HealthType::Maint => self.spot_markets[token_index].maint_asset_weight,
				HealthType::Init => self.spot_markets[token_index].init_asset_weight,
				HealthType::Equity => ONE_I80F48,
			}
		}
	}
}

/// This is the root bank for one token's lending and borrowing info
#[derive(Copy, Clone, Pod, Loadable)]
#[repr(C)]
pub struct RootBank {
	pub meta_data: MetaData,
	
	pub optimal_util: I80F48,
	pub optimal_rate: I80F48,
	pub max_rate: I80F48,
	
	pub num_node_banks: usize,
	pub node_banks: [Pubkey; MAX_NODE_BANKS],
	
	pub deposit_index: I80F48,
	pub borrow_index: I80F48,
	pub last_updated: u64,
	
	padding: [u8; 64], // used for future expansions
}

impl RootBank {
	
	pub fn set_rate_params(
		&mut self,
		optimal_util: I80F48,
		optimal_rate: I80F48,
		max_rate: I80F48,
	) -> MangoResult<()> {
		
		self.optimal_util = optimal_util;
		self.optimal_rate = optimal_rate;
		self.max_rate = max_rate;
		
		Ok(())
	}
	pub fn load_mut_checked(
		account: AccountInfo,
		program_id: &Pubkey,
	) -> MangoResult<Self> {
		
		let root_bank = Self::load_from_bytes(&account.data)?;
		
		
		Ok(root_bank.clone())
	}
	pub fn load_checked(
		account: AccountInfo,
		program_id: &Pubkey,
	) -> MangoResult<Self> {
		
		let root_bank = Self::load_from_bytes(&account.data)?;
		
		
		Ok(root_bank.clone())
	}
	pub fn find_node_bank_index(&self, node_bank_pk: &Pubkey) -> Option<usize> {
		self.node_banks.iter().position(|pk| pk == node_bank_pk)
	}
	
	pub fn update_index(
		&mut self,
		node_bank_ais: &[AccountInfo],
		program_id: &Pubkey,
		now_ts: u64,
	) -> MangoResult<()> {
		let mut native_deposits = ZERO_I80F48;
		let mut native_borrows = ZERO_I80F48;
		
		for node_bank_ai in node_bank_ais.iter() {
			let node_bank = NodeBank::load_from_bytes(&node_bank_ai.data)?;
			native_deposits = native_deposits
				  .checked_add(node_bank.deposits.checked_mul(self.deposit_index).unwrap())
				  .unwrap();
			native_borrows = native_borrows
				  .checked_add(node_bank.borrows.checked_mul(self.borrow_index).unwrap())
				  .unwrap();
		}
		
		// TODO - is this a good assumption?
		let utilization = native_borrows.checked_div(native_deposits).unwrap_or(ZERO_I80F48);
		
		// Calculate interest rate
		let interest_rate = compute_interest_rate(&self, utilization);
		
		let borrow_interest: I80F48 =
			  interest_rate.checked_mul(I80F48::from_num(now_ts - self.last_updated)).unwrap();
		let deposit_interest = borrow_interest.checked_mul(utilization).unwrap();
		
		self.last_updated = now_ts;
		if borrow_interest <= ZERO_I80F48 || deposit_interest <= ZERO_I80F48 {
			return Ok(());
		}
		self.borrow_index = self
			  .borrow_index
			  .checked_mul(borrow_interest)
			  .unwrap()
			  .checked_div(YEAR)
			  .unwrap()
			  .checked_add(self.borrow_index)
			  .unwrap();
		self.deposit_index = self
			  .deposit_index
			  .checked_mul(deposit_interest)
			  .unwrap()
			  .checked_div(YEAR)
			  .unwrap()
			  .checked_add(self.deposit_index)
			  .unwrap();
		
		Ok(())
	}
	
	/// Socialize the loss on lenders and return (native_loss, percentage_loss)
	pub fn socialize_loss(
		&mut self,
		program_id: &Pubkey,
		token_index: usize,
		mango_cache: &mut MangoCache,
		bankrupt_account: &mut MangoAccount,
		node_bank_ais: &[AccountInfo; MAX_NODE_BANKS],
	) -> MangoResult<(I80F48, I80F48)> {
		let mut static_deposits = ZERO_I80F48;
		
		for i in 0..self.num_node_banks {
			
			let node_bank = NodeBank::load_from_bytes(&node_bank_ais[i].data)?;
			static_deposits = static_deposits.checked_add(node_bank.deposits).unwrap();
		}
		
		let native_deposits = static_deposits.checked_mul(self.deposit_index).unwrap();
		let mut loss = bankrupt_account.borrows[token_index];
		let native_loss: I80F48 = loss * self.borrow_index;
		
		// TODO what if loss is greater than entire native deposits
		let percentage_loss = native_loss.checked_div(native_deposits).unwrap();
		self.deposit_index = self
			  .deposit_index
			  .checked_sub(percentage_loss.checked_mul(self.deposit_index).unwrap())
			  .unwrap();
		
		mango_cache.root_bank_cache[token_index].deposit_index = self.deposit_index;
		
		// // Reduce borrows on the bankrupt_account; Spread out over node banks if necessary
		// for i in 0..self.num_node_banks {
		// 	let mut node_bank = NodeBank::load_from_bytes(&node_bank_ais[i].data)?;
		// 	let node_loss = loss.min(node_bank.borrows);
		// 	bankrupt_account.checked_sub_borrow(token_index, node_loss)?;
		// 	node_bank.checked_sub_borrow(node_loss)?;
		// 	loss -= node_loss;
		// 	if loss.is_zero() {
		// 		break;
		// 	}
		// }
		Ok((native_loss, percentage_loss))
	}
}

#[derive(Copy, Clone, Pod, Loadable)]
#[repr(C)]
pub struct NodeBank {
	pub meta_data: MetaData,
	
	pub deposits: I80F48,
	pub borrows: I80F48,
	pub vault: Pubkey,
}

impl NodeBank {
	
	pub fn load_mut_checked(
		account: AccountInfo,
		program_id: &Pubkey,
	) -> MangoResult<Self> {
		let node_bank = Self::load_from_bytes(&account.data)?;
		
		
		Ok(node_bank.clone())
	}
	
	pub fn load_checked(
		account: AccountInfo,
		program_id: &Pubkey,
	) -> MangoResult<Self> {
		let node_bank = Self::load_from_bytes(&account.data)?;
		

		
		Ok(node_bank.clone())
	}
	
	// TODO - Add checks to these math methods to prevent result from being < 0
	pub fn checked_add_borrow(&mut self, v: I80F48) -> MangoResult<()> {
		Ok(self.borrows = self.borrows.checked_add(v).unwrap())
	}
	pub fn checked_sub_borrow(&mut self, v: I80F48) -> MangoResult<()> {
		Ok(self.borrows = self.borrows.checked_sub(v).unwrap())
	}
	pub fn checked_add_deposit(&mut self, v: I80F48) -> MangoResult<()> {
		Ok(self.deposits = self.deposits.checked_add(v).unwrap())
	}
	pub fn checked_sub_deposit(&mut self, v: I80F48) -> MangoResult<()> {
		Ok(self.deposits = self.deposits.checked_sub(v).unwrap())
	}
	pub fn has_valid_deposits_borrows(&self, root_bank_cache: &RootBankCache) -> bool {
		self.get_total_native_deposit(root_bank_cache)
			  >= self.get_total_native_borrow(root_bank_cache)
	}
	pub fn get_total_native_borrow(&self, root_bank_cache: &RootBankCache) -> u64 {
		let native: I80F48 = self.borrows * root_bank_cache.borrow_index;
		native.checked_ceil().unwrap().checked_to_num().unwrap() // rounds toward +inf
	}
	pub fn get_total_native_deposit(&self, root_bank_cache: &RootBankCache) -> u64 {
		let native: I80F48 = self.deposits * root_bank_cache.deposit_index;
		native.checked_floor().unwrap().checked_to_num().unwrap() // rounds toward -inf
	}
}

#[derive(Copy, Clone, Pod)]
#[repr(C)]
pub struct PriceCache {
	pub price: I80F48, // unit is interpreted as how many quote native tokens for 1 base native token
	pub last_update: u64,
}

impl PriceCache {
	pub fn check_valid(&self, mango_group: &MangoGroup, now_ts: u64) -> MangoResult<()> {
		// Hack: explicitly double valid_interval as a quick fix to make Mango
		// less likely to become unusable when solana reliability goes bad.
		// There's currently no instruction to change the valid_interval.
	Ok(())
	}
}

#[derive(Copy, Clone, Pod)]
#[repr(C)]
pub struct RootBankCache {
	pub deposit_index: I80F48,
	pub borrow_index: I80F48,
	pub last_update: u64,
}

impl RootBankCache {
	pub fn check_valid(&self, mango_group: &MangoGroup, now_ts: u64) -> MangoResult<()> {
		Ok(())
		
	}
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PerpMarketData {
	pub name: String,
	pub pubkey: String,
	pub base_symbol: String,
	pub base_decimals: u8,
	pub quote_decimals: u8,
	pub market_index: usize,
	pub bids_key: String,
	pub asks_key: String,
	pub events_key: String
}
impl PerpMarketData {
	pub fn ui_to_quote_units(&self, quantity: f64) -> f64{
		return 10_f64.powf(self.quote_decimals as f64) * quantity
	}
	pub fn ui_to_base_units(&self, quantity: f64) -> f64{
		return 10_f64.powf(self.base_decimals as f64) * quantity
	}
	pub fn lotToNativePriceQuantity(&self, price: u64, quantity: u64) -> (i64, i64) {
		return (0 as i64, 0 as i64);
	}
}
#[derive(Copy, Clone, Pod)]
#[repr(C)]
pub struct PerpMarketCache {
	pub long_funding: I80F48,
	pub short_funding: I80F48,
	pub last_update: u64,
}

impl PerpMarketCache {
	pub fn check_valid(&self, mango_group: &MangoGroup, now_ts: u64) -> MangoResult<()> {
		Ok(())
		
	}
}

#[derive(Copy, Clone, Pod, Loadable)]
#[repr(C)]
pub struct MangoCache {
	pub meta_data: MetaData,
	
	pub price_cache: [PriceCache; MAX_PAIRS],
	pub root_bank_cache: [RootBankCache; MAX_TOKENS],
	pub perp_market_cache: [PerpMarketCache; MAX_PAIRS],
}

impl MangoCache {
	pub fn load_mut_checked(
		account: AccountInfo,
		program_id: &Pubkey,
		mango_group: &MangoGroup,
	) -> MangoResult<Self> {
		// mango account must be rent exempt to even be initialized
		let mango_cache = Self::load_from_bytes(&account.data)?;
		
		
		Ok(mango_cache.clone())
	}
	
	pub fn load_checked(
		account: AccountInfo,
		program_id: &Pubkey,
		mango_group: &MangoGroup,
	) -> MangoResult<Self> {
		
		let mango_cache = Self::load_from_bytes(&account.data)?;
		
		
		Ok(mango_cache.clone())
	}
	
	pub fn check_valid(
		&self,
		mango_group: &MangoGroup,
		active_assets: &UserActiveAssets,
		now_ts: u64,
	) -> MangoResult<()> {
		for i in 0..mango_group.num_oracles {
			if active_assets.spot[i] || active_assets.perps[i] {
				self.price_cache[i].check_valid(&mango_group, now_ts)?;
			}
			
			if active_assets.spot[i] {
				self.root_bank_cache[i].check_valid(&mango_group, now_ts)?;
			}
			
			if active_assets.perps[i] {
				self.perp_market_cache[i].check_valid(&mango_group, now_ts)?;
			}
		}
		self.root_bank_cache[QUOTE_INDEX].check_valid(&mango_group, now_ts)
	}
	
	pub fn get_price(&self, i: usize) -> f64 {
		if i == QUOTE_INDEX {
			1_f64
		} else {
			self.price_cache[i].price.to_string().parse::<f64>().unwrap() // Just panic if index out of bounds
		}
	}
}

#[derive(Debug)]
pub struct UserActiveAssets {
	pub spot: [bool; MAX_PAIRS],
	pub perps: [bool; MAX_PAIRS],
}

impl UserActiveAssets {
	pub fn new(
		mango_group: &MangoGroup,
		mango_account: &MangoAccount,
		extra: Vec<(AssetType, usize)>,
	) -> Self {
		let mut spot = [false; MAX_PAIRS];
		let mut perps = [false; MAX_PAIRS];
		for i in 0..mango_group.num_oracles {
			spot[i] = !mango_group.spot_markets[i].is_empty()
				  && (mango_account.in_margin_basket[i]
				  || !mango_account.deposits[i].is_zero()
				  || !mango_account.borrows[i].is_zero());
			
			perps[i] = !mango_group.perp_markets[i].is_empty()
				  && mango_account.perp_accounts[i].is_active();
		}
		extra.iter().for_each(|(at, i)| match at {
			AssetType::Token => {
				if *i != QUOTE_INDEX {
					spot[*i] = true;
				}
			}
			AssetType::Perp => {
				perps[*i] = true;
			}
		});
		Self { spot, perps }
	}
	
	pub fn merge(a: &Self, b: &Self) -> Self {
		let mut spot = [false; MAX_PAIRS];
		let mut perps = [false; MAX_PAIRS];
		for i in 0..MAX_PAIRS {
			spot[i] = a.spot[i] || b.spot[i];
			perps[i] = a.perps[i] || b.perps[i];
		}
		Self { spot, perps }
	}
}

pub struct HealthCache {
	pub active_assets: UserActiveAssets,
	
	/// Vec of length MAX_PAIRS containing worst case spot vals; unweighted
	spot: Vec<(I80F48, I80F48)>,
	perp: Vec<(I80F48, I80F48)>,
	quote: I80F48,
	
	/// This will be zero until update_health is called for the first time
	health: [Option<I80F48>; NUM_HEALTHS],
}

fn strip_dex_padding(acc: AccountInfo) -> MangoResult<Vec<u8>> {
	let data = acc.data;
	let data_len = data.len() - 12;
	let (_, rest) = data.split_at(5);
	let (mid, _) = rest.split_at(data_len);
	Ok(Vec::from(mid))
}

pub fn load_open_orders(
	acc: AccountInfo,
) -> Result<serum_dex::state::OpenOrders, ProgramError> {
	Ok(from_bytes::<OpenOrders>(&strip_dex_padding(acc)?).clone())
}
impl HealthCache {
	pub fn new(active_assets: UserActiveAssets) -> Self {
		Self {
			active_assets,
			spot: vec![(ZERO_I80F48, ZERO_I80F48); MAX_PAIRS],
			perp: vec![(ZERO_I80F48, ZERO_I80F48); MAX_PAIRS],
			quote: ZERO_I80F48,
			health: [None; NUM_HEALTHS],
		}
	}
	
	// Accept T = &OpenOrders as well as Ref<OpenOrders>
	pub fn init_vals_with_orders_vec(
		&mut self,
		mango_group: &MangoGroup,
		mango_cache: &MangoCache,
		mango_account: &MangoAccount,
		open_orders: &[Option<OpenOrders>],
	) -> MangoResult<()> {
		self.quote = mango_account.get_net(&mango_cache.root_bank_cache[QUOTE_INDEX], QUOTE_INDEX);
		for i in 0..mango_group.num_oracles {
			if self.active_assets.spot[i] {
				self.spot[i] = mango_account.get_spot_val(
					&mango_cache.root_bank_cache[i],
					mango_cache.price_cache[i].price,
					i,
					&open_orders[i],
				)?;
			}
			
			if self.active_assets.perps[i] {
				self.perp[i] = mango_account.perp_accounts[i].get_val(
					&mango_group.perp_markets[i],
					&mango_cache.perp_market_cache[i],
					mango_cache.price_cache[i].price,
				)?;
			}
		}
		Ok(())
	}
	
	pub fn get_health(&mut self, mango_group: &MangoGroup, health_type: HealthType) -> I80F48 {
		let health_index = health_type as usize;
		match self.health[health_index] {
			None => {
				// apply weights, cache result, return health
				let mut health = self.quote;
				for i in 0..mango_group.num_oracles {
					let spot_market_info = &mango_group.spot_markets[i];
					let perp_market_info = &mango_group.perp_markets[i];
					
					let (spot_asset_weight, spot_liab_weight, perp_asset_weight, perp_liab_weight) =
						  match health_type {
							  HealthType::Maint => (
								  spot_market_info.maint_asset_weight,
								  spot_market_info.maint_liab_weight,
								  perp_market_info.maint_asset_weight,
								  perp_market_info.maint_liab_weight,
							  ),
							  HealthType::Init => (
								  spot_market_info.init_asset_weight,
								  spot_market_info.init_liab_weight,
								  perp_market_info.init_asset_weight,
								  perp_market_info.init_liab_weight,
							  ),
							  HealthType::Equity => (ONE_I80F48, ONE_I80F48, ONE_I80F48, ONE_I80F48),
						  };
					
					if self.active_assets.spot[i] {
						let (base, quote) = self.spot[i];
						if base.is_negative() {
							health += base * spot_liab_weight + quote;
						} else {
							health += base * spot_asset_weight + quote
						}
					}
					
					if self.active_assets.perps[i] {
						let (base, quote) = self.perp[i];
						if base.is_negative() {
							health += base * perp_liab_weight + quote;
						} else {
							health += base * perp_asset_weight + quote
						}
					}
				}
				
				self.health[health_index] = Some(health);
				health
			}
			Some(h) => h,
		}
	}
	
	#[cfg(feature = "client")]
	pub fn get_health_components(
		&mut self,
		mango_group: &MangoGroup,
		health_type: HealthType,
	) -> (I80F48, I80F48) {
		let (mut assets, mut liabilities) = if self.quote.is_negative() {
			(ZERO_I80F48, -self.quote)
		} else {
			(self.quote, ZERO_I80F48)
		};
		for i in 0..mango_group.num_oracles {
			let spot_market_info = &mango_group.spot_markets[i];
			let perp_market_info = &mango_group.perp_markets[i];
			
			let (spot_asset_weight, spot_liab_weight, perp_asset_weight, perp_liab_weight) =
				  match health_type {
					  HealthType::Maint => (
						  spot_market_info.maint_asset_weight,
						  spot_market_info.maint_liab_weight,
						  perp_market_info.maint_asset_weight,
						  perp_market_info.maint_liab_weight,
					  ),
					  HealthType::Init => (
						  spot_market_info.init_asset_weight,
						  spot_market_info.init_liab_weight,
						  perp_market_info.init_asset_weight,
						  perp_market_info.init_liab_weight,
					  ),
					  HealthType::Equity => (ONE_I80F48, ONE_I80F48, ONE_I80F48, ONE_I80F48),
				  };
			
			if self.active_assets.spot[i] {
				let (base, quote) = self.spot[i];
				if quote.is_negative() {
					liabilities -= quote;
				} else {
					assets += quote;
				}
				if base.is_negative() {
					liabilities -= base * spot_liab_weight;
				} else {
					assets += base * spot_asset_weight;
				}
			}
			
			if self.active_assets.perps[i] {
				let (base, quote) = self.perp[i];
				if quote.is_negative() {
					liabilities -= quote;
				} else {
					assets += quote;
				}
				if base.is_negative() {
					liabilities -= base * perp_liab_weight;
				} else {
					assets += base * perp_asset_weight;
				}
			}
		}
		
		(assets, liabilities)
	}
	
	pub fn update_quote(&mut self, mango_cache: &MangoCache, mango_account: &MangoAccount) {
		let quote = mango_account.get_net(&mango_cache.root_bank_cache[QUOTE_INDEX], QUOTE_INDEX);
		for i in 0..NUM_HEALTHS {
			if let Some(h) = self.health[i] {
				self.health[i] = Some(h + quote - self.quote);
			}
		}
		self.quote = quote;
	}
	
	
	/// Simulate health after changes to taker base, taker quote, bids quantity and asks quantity
	pub fn get_health_after_sim_perp(
		&self,
		mango_group: &MangoGroup,
		mango_cache: &MangoCache,
		mango_account: &MangoAccount,
		market_index: usize,
		health_type: HealthType,
		
		taker_base: i64,
		taker_quote: i64,
		bids_quantity: i64,
		asks_quantity: i64,
	) -> MangoResult<I80F48> {
		let info = &mango_group.perp_markets[market_index];
		let (base, quote) = mango_account.perp_accounts[market_index].sim_get_val(
			info,
			&mango_cache.perp_market_cache[market_index],
			mango_cache.price_cache[market_index].price,
			taker_base,
			taker_quote,
			bids_quantity,
			asks_quantity,
		)?;
		
		let (prev_base, prev_quote) = self.perp[market_index];
		let pmi = &mango_group.perp_markets[market_index];
		
		let (asset_weight, liab_weight) = match health_type {
			HealthType::Maint => (pmi.maint_asset_weight, pmi.maint_liab_weight),
			HealthType::Init => (pmi.init_asset_weight, pmi.init_liab_weight),
			HealthType::Equity => (ONE_I80F48, ONE_I80F48),
		};
		
		// Get health from val
		let prev_perp_health = if prev_base.is_negative() {
			prev_base * liab_weight + prev_quote
		} else {
			prev_base * asset_weight + prev_quote
		};
		
		let curr_perp_health = if base.is_negative() {
			base * liab_weight + quote
		} else {
			base * asset_weight + quote
		};
		
		let h = self.health[health_type as usize].unwrap();
		
		// Apply taker fees; Assume no referrer
		let taker_fees = if taker_quote != 0 {
			let taker_quote_native =
				  I80F48::from_num(info.quote_lot_size.checked_mul(taker_quote.abs()).unwrap());
			let mut market_fees = info.taker_fee * taker_quote_native;
			if let Some(mngo_index) = mango_group.find_token_index(&mngo_token::id()) {
				let mngo_cache = &mango_cache.root_bank_cache[mngo_index];
				let mngo_deposits = mango_account.get_native_deposit(mngo_cache, mngo_index)?;
				let ref_mngo_req = I80F48::from_num(mango_group.ref_mngo_required);
				if mngo_deposits < ref_mngo_req {
					market_fees += (I80F48::from_num(mango_group.ref_surcharge_centibps)
						  / CENTIBPS_PER_UNIT)
						  * taker_quote_native;
				}
			}
			market_fees
		} else {
			ZERO_I80F48
		};
		Ok(h + curr_perp_health - prev_perp_health - taker_fees)
	}
	
	/// Update perp val and then update the healths
	pub fn update_perp_val(
		&mut self,
		mango_group: &MangoGroup,
		mango_cache: &MangoCache,
		mango_account: &MangoAccount,
		market_index: usize,
	) -> MangoResult<()> {
		let (base, quote) = mango_account.perp_accounts[market_index].get_val(
			&mango_group.perp_markets[market_index],
			&mango_cache.perp_market_cache[market_index],
			mango_cache.price_cache[market_index].price,
		)?;
		
		let (prev_base, prev_quote) = self.perp[market_index];
		
		for i in 0..NUM_HEALTHS {
			if let Some(h) = self.health[i] {
				let health_type: HealthType = HealthType::try_from_primitive(i).unwrap();
				let pmi = &mango_group.perp_markets[market_index];
				
				let (asset_weight, liab_weight) = match health_type {
					HealthType::Maint => (pmi.maint_asset_weight, pmi.maint_liab_weight),
					HealthType::Init => (pmi.init_asset_weight, pmi.init_liab_weight),
					HealthType::Equity => (ONE_I80F48, ONE_I80F48),
				};
				
				// Get health from val
				let prev_perp_health = if prev_base.is_negative() {
					prev_base * liab_weight + prev_quote
				} else {
					prev_base * asset_weight + prev_quote
				};
				
				let curr_perp_health = if base.is_negative() {
					base * liab_weight + quote
				} else {
					base * asset_weight + quote
				};
				
				self.health[i] = Some(h + curr_perp_health - prev_perp_health);
			}
		}
		
		self.perp[market_index] = (base, quote);
		
		Ok(())
	}
}

#[derive(Copy, Debug, Clone, Pod, Loadable)]
#[repr(C)]
pub struct MangoAccount {
	pub meta_data: MetaData, // 8
	
	pub mango_group: Pubkey, // 40
	pub owner: Pubkey,
	
	pub in_margin_basket: [bool; MAX_PAIRS], // 87
	pub num_in_margin_basket: u8,
	
	// Spot and Margin related data
	pub deposits: [I80F48; MAX_TOKENS],
	pub borrows: [I80F48; MAX_TOKENS],
	pub spot_open_orders: [Pubkey; MAX_PAIRS],
	
	// Perps related data
	pub perp_accounts: [PerpAccount; MAX_PAIRS],
	
	pub order_market: [u8; MAX_PERP_OPEN_ORDERS],
	pub order_side: [Side; MAX_PERP_OPEN_ORDERS],
	pub orders: [i128; MAX_PERP_OPEN_ORDERS],
	pub client_order_ids: [u64; MAX_PERP_OPEN_ORDERS],
	
	pub msrm_amount: u64,
	
	/// This account cannot open new positions or borrow until `init_health >= 0`
	pub being_liquidated: bool,
	
	/// This account cannot do anything except go through `resolve_bankruptcy`
	pub is_bankrupt: bool,
	pub info: [u8; INFO_LEN],
	
	/// Starts off as zero pubkey and points to the AdvancedOrders account
	pub advanced_orders_key: Pubkey,
	
	/// Can this account be upgraded to v1 so it can be closed
	pub not_upgradable: bool,
	
	// Alternative authority/signer of transactions for a mango account
	pub delegate: Pubkey,
	
	/// padding for expansions
	/// Note: future expansion can also be just done via isolated PDAs
	/// which can be computed independently and dont need to be linked from
	/// this account
	pub padding: [u8; 5],
}

impl MangoAccount {
	pub fn load_from_vec(data: Vec<u8>) -> MangoResult<Self> {
		Ok(Self::load_from_bytes(&data).unwrap().clone())
	}
	pub fn load_mut_checked(
		account: AccountInfo,
		program_id: &Pubkey,
		mango_group_pk: &Pubkey,
	) -> MangoResult<Self> {
		// load_mut checks for size already
		let mango_account = Self::load_from_bytes(&account.data)?;
		Ok(mango_account.clone())
	}
	pub fn load_checked(
		account: AccountInfo,
		program_id: &Pubkey,
	) -> MangoResult<Self> {
		let mango_account = Self::load_from_bytes(&account.data)?;
		
		Ok(mango_account.clone())
	}
	pub fn get_native_deposit(
		&self,
		root_bank_cache: &RootBankCache,
		token_i: usize,
	) -> MangoResult<I80F48> {
		Ok(self.deposits[token_i].checked_mul(root_bank_cache.deposit_index).unwrap())
	}
	pub fn get_native_borrow(
		&self,
		root_bank_cache: &RootBankCache,
		token_i: usize,
	) -> MangoResult<I80F48> {
		Ok(self.borrows[token_i].checked_mul(root_bank_cache.borrow_index).unwrap())
	}
	
	// TODO - Add unchecked versions to be used when we're confident
	// TODO OPT - remove negative and zero checks if we're confident
	pub fn checked_add_borrow(&mut self, token_i: usize, v: I80F48) -> MangoResult<()> {
		self.borrows[token_i] = self.borrows[token_i].checked_add(v).unwrap();
		Ok(())
	}
	pub fn checked_sub_borrow(&mut self, token_i: usize, v: I80F48) -> MangoResult<()> {
		self.borrows[token_i] = self.borrows[token_i].checked_sub(v).unwrap();
		Ok(())
	}
	pub fn checked_add_deposit(&mut self, token_i: usize, v: I80F48) -> MangoResult<()> {
		self.deposits[token_i] = self.deposits[token_i].checked_add(v).unwrap();
		Ok(())
	}
	pub fn checked_sub_deposit(&mut self, token_i: usize, v: I80F48) -> MangoResult<()> {
		self.deposits[token_i] = self.deposits[token_i].checked_sub(v).unwrap();
		Ok(())
	}
	
	pub fn get_net(&self, bank_cache: &RootBankCache, token_index: usize) -> I80F48 {
		if self.deposits[token_index].is_positive() {
			self.deposits[token_index].checked_mul(bank_cache.deposit_index).unwrap()
		} else if self.borrows[token_index].is_positive() {
			-self.borrows[token_index].checked_mul(bank_cache.borrow_index).unwrap()
		} else {
			ZERO_I80F48
		}
	}
	
	/// Return the token value and quote token value for this market taking into account open order
	/// but not doing asset weighting
	#[inline(always)]
	fn get_spot_val(
		&self,
		bank_cache: &RootBankCache,
		price: I80F48,
		market_index: usize,
		open_orders: &Option<OpenOrders>,
	) -> MangoResult<(I80F48, I80F48)> {
		let base_net = self.get_net(bank_cache, market_index);
		if !self.in_margin_basket[market_index] || open_orders.is_none() {
			Ok((base_net * price, ZERO_I80F48))
		} else {
			let (quote_free, quote_locked, base_free, base_locked) =
				  split_open_orders(open_orders.as_ref().unwrap().deref());
			
			// Two "worst-case" scenarios are considered:
			// 1. All bids are executed at current price, producing a base amount of bids_base_net
			//    when all quote_locked are converted to base.
			// 2. All asks are executed at current price, producing a base amount of asks_base_net
			//    because base_locked would be converted to quote.
			let bids_base_net: I80F48 = base_net + base_free + base_locked + quote_locked / price;
			let asks_base_net = base_net + base_free;
			
			// Report the scenario that would have a worse outcome on health.
			//
			// Explanation: This function returns (base, quote) and the values later get used in
			//     health += (if base > 0 { asset_weight } else { liab_weight }) * base + quote
			// and here we return the scenario that will increase health the least.
			//
			// Correctness proof:
			// - always bids_base_net >= asks_base_net
			// - note that scenario 1 returns (a + b, c)
			//         and scenario 2 returns (a,     c + b), and b >= 0, c >= 0
			// - if a >= 0: scenario 1 will lead to less health as asset_weight <= 1.
			// - if a < 0 and b <= -a: scenario 2 will lead to less health as liab_weight >= 1.
			// - if a < 0 and b > -a:
			//   The health contributions of both scenarios are identical if
			//       asset_weight * (a + b) + c = liab_weight * a + c + b
			//   <=> b = (asset_weight - liab_weight) / (1 - asset_weight) * a
			//   <=> b = -2 a  since asset_weight + liab_weight = 2 by weight construction
			//   So the worse scenario switches when a + b = -a.
			// That means scenario 1 leads to less health whenever |a + b| > |a|.
			
			if bids_base_net.abs() > asks_base_net.abs() {
				Ok((bids_base_net * price, quote_free))
			} else {
				Ok((asks_base_net * price, base_locked * price + quote_free + quote_locked))
			}
		}
	}
	
	/// Add a market to margin basket
	/// This function should be called any time you place a spot order
	pub fn add_to_basket(&mut self, market_index: usize) -> MangoResult<()> {
		if self.num_in_margin_basket == MAX_NUM_IN_MARGIN_BASKET {
			Ok(())
		} else {
			if !self.in_margin_basket[market_index] {
				self.in_margin_basket[market_index] = true;
				self.num_in_margin_basket += 1;
			}
			Ok(())
		}
	}
	
	/// Determine if margin basket should be updated.
	/// This function should be called any time you settle funds on serum dex
	pub fn update_basket(
		&mut self,
		market_index: usize,
		open_orders: &serum_dex::state::OpenOrders,
	) -> MangoResult {
		let is_empty = open_orders.native_pc_total == 0
			  && open_orders.native_coin_total == 0
			  && open_orders.referrer_rebates_accrued == 0
			  && open_orders.free_slot_bits == u128::MAX;
		
		if self.in_margin_basket[market_index] && is_empty {
			self.in_margin_basket[market_index] = false;
			self.num_in_margin_basket -= 1;
		} else if !self.in_margin_basket[market_index] && !is_empty {
			self.in_margin_basket[market_index] = true;
			self.num_in_margin_basket += 1;
		}
		Ok(())
	}
	
	
	/// *** Below are methods related to the perps open orders ***
	pub fn next_order_slot(&self) -> Option<usize> {
		self.order_market.iter().position(|&i| i == FREE_ORDER_SLOT)
	}

	///
	pub fn remove_order(&mut self, slot: usize, quantity: i64) -> MangoResult<()> {
		let market_index = self.order_market[slot] as usize;
		
		// accounting
		match self.order_side[slot] {
			Side::Bid => {
				self.perp_accounts[market_index].bids_quantity -= quantity;
			}
			Side::Ask => {
				self.perp_accounts[market_index].asks_quantity -= quantity;
			}
		}
		
		// release space
		self.order_market[slot] = FREE_ORDER_SLOT;
		
		// TODO OPT - remove these; unnecessary
		self.order_side[slot] = Side::Bid;
		self.orders[slot] = 0i128;
		self.client_order_ids[slot] = 0u64;
		Ok(())
	}
	

	pub fn find_order_with_client_id(
		&self,
		market_index: usize,
		client_id: u64,
	) -> Option<(i128, Side)> {
		let market_index = market_index as u8;
		for i in 0..MAX_PERP_OPEN_ORDERS {
			if self.order_market[i] == market_index && self.client_order_ids[i] == client_id {
				return Some((self.orders[i], self.order_side[i]));
			}
		}
		None
	}
	pub fn find_order_side(&self, market_index: usize, order_id: i128) -> Option<Side> {
		let market_index = market_index as u8;
		for i in 0..MAX_PERP_OPEN_ORDERS {
			if self.order_market[i] == market_index && self.orders[i] == order_id {
				return Some(self.order_side[i]);
			}
		}
		None
	}
	
	// pub fn max_withdrawable(
	// 	&self,
	// 	group: &MangoGroup,
	// 	mango_cache: &MangoCache,
	// 	token_index: usize,
	// 	health: I80F48,
	// ) -> MangoResult<u64> {
	// 	if health.is_positive() && self.deposits[token_index].is_positive() {
	// 		let price = mango_cache.get_price(token_index);
	// 		let init_asset_weight = group.get_token_asset_weight(token_index, HealthType::Init);
	// 		let health_implied = (health / (price * init_asset_weight)).checked_floor().unwrap();
	// 		let native_deposits: I80F48 = self
	// 			  .get_native_deposit(&mango_cache.root_bank_cache[token_index], token_index)?
	// 			  .checked_floor()
	// 			  .unwrap();
	// 		Ok(native_deposits.min(health_implied).to_num())
	// 	} else {
	// 		Ok(0)
	// 	}
	// }
}
#[derive(Copy, Clone, Debug, Pod)]
#[repr(C)]
pub struct PerpAccount {
	pub base_position: i64,     // measured in base lots
	pub quote_position: I80F48, // measured in native quote
	
	pub long_settled_funding: I80F48,
	pub short_settled_funding: I80F48,
	
	// orders related info
	pub bids_quantity: i64, // total contracts in sell orders
	pub asks_quantity: i64, // total quote currency in buy orders
	
	/// Amount that's on EventQueue waiting to be processed
	pub taker_base: i64,
	pub taker_quote: i64,
	
	pub mngo_accrued: u64,
}

impl PerpAccount {
	/// Add taker trade after it has been matched but before it has been process on EventQueue
	pub fn add_taker_trade(&mut self, base_change: i64, quote_change: i64) {
		// TODO make checked? estimate chances of overflow here
		self.taker_base += base_change;
		self.taker_quote += quote_change;
	}
	/// Remove taker trade after it has been processed on EventQueue
	pub fn remove_taker_trade(&mut self, base_change: i64, quote_change: i64) {
		self.taker_base -= base_change;
		self.taker_quote -= quote_change;
	}
	
	fn convert_points(
		&mut self,
		lmi: &mut LiquidityMiningInfo,
		time_final: u64,
		mut points: I80F48,
	) {
		let points_in_period = I80F48::from_num(lmi.mngo_left).checked_div(lmi.rate).unwrap();
		
		if points >= points_in_period {
			self.mngo_accrued += lmi.mngo_left;
			points -= points_in_period;
			
			let rate_adj = I80F48::from_num(time_final - lmi.period_start)
				  .checked_div(I80F48::from_num(lmi.target_period_length))
				  .unwrap()
				  .clamp(MIN_RATE_ADJ, MAX_RATE_ADJ);
			
			lmi.rate = lmi.rate.checked_mul(rate_adj).unwrap();
			lmi.period_start = time_final;
			lmi.mngo_left = lmi.mngo_per_period;
		}
		
		let mngo_earned =
			  points.checked_mul(lmi.rate).unwrap().to_num::<u64>().min(lmi.mngo_per_period); // limit mngo payout to max mngo in a period
		
		self.mngo_accrued += mngo_earned;
		lmi.mngo_left -= mngo_earned;
	}
	
	/// New form of incentives introduced in v3.2. This will apply incentives to the top N contracts
	pub fn apply_size_incentives(
		&mut self,
		perp_market: &mut PerpMarket,
		best_initial: i64,
		best_final: i64,
		time_initial: u64,
		time_final: u64,
		quantity: i64,
	) -> MangoResult {
		let lmi = &mut perp_market.liquidity_mining_info;
		if lmi.rate == 0 || lmi.mngo_per_period == 0 {
			return Ok(());
		}
		
		// TODO - consider limiting time instead of choosing the worse of two positions
		let time_factor = I80F48::from_num((time_final - time_initial).min(864_000));
		
		// reinterpreted as number of contracts
		// TODO - max_depth_bps must be some number between 1 - 100 so there are no overflows on high exp
		//      maybe on overflow we just set points equal to max?
		let max_depth_size = lmi.max_depth_bps;
		let size_dist = I80F48::from_num(best_final.max(best_initial));
		let size_dist_factor = max_depth_size - size_dist;
		if !size_dist_factor.is_positive() {
			return Ok(());
		}
		
		let quantity = I80F48::from_num(quantity).min(size_dist_factor);
		let exp = perp_market.meta_data.extra_info[0];
		let lm_size_shift = perp_market.meta_data.extra_info[1];
		let size_dist_factor = size_dist_factor >> lm_size_shift;
		let points = pow_i80f48(size_dist_factor, exp)
			  .checked_mul(time_factor)
			  .unwrap()
			  .checked_mul(quantity)
			  .unwrap();
		
		self.convert_points(lmi, time_final, points);
		
		Ok(())
	}
	pub fn apply_price_incentives(
		&mut self,
		perp_market: &mut PerpMarket,
		
		side: Side,
		price: i64,
		best_initial: i64,
		best_final: i64,
		time_initial: u64,
		time_final: u64,
		quantity: i64,
	) -> MangoResult {
		// TODO v3.2 depending on perp market version apply incentives in different way
		let lmi = &mut perp_market.liquidity_mining_info;
		if lmi.rate == 0 || lmi.mngo_per_period == 0 {
			return Ok(());
		}
		
		let best = match side {
			Side::Bid => max(best_initial, best_final),
			Side::Ask => min(best_initial, best_final),
		};
		
		// TODO limit incentives to orders that were on book at least 5 seconds
		// cap time_final - time_initial to 864_000 ~= 10 days this is to prevent overflow
		let time_factor = I80F48::from_num((time_final - time_initial).min(864_000));
		let quantity = I80F48::from_num(quantity);
		
		// special case that only rewards top of book
		let points = if lmi.max_depth_bps.is_zero() {
			if best == price {
				time_factor.checked_mul(quantity).unwrap()
			} else {
				return Ok(());
			}
		} else {
			let dist_bps = I80F48::from_num((best - price).abs() * 10_000) / I80F48::from_num(best);
			let dist_factor: I80F48 = max(lmi.max_depth_bps - dist_bps, ZERO_I80F48);
			pow_i80f48(dist_factor, perp_market.meta_data.extra_info[0])
				  .checked_mul(time_factor)
				  .unwrap()
				  .checked_mul(quantity)
				  .unwrap()
		};
		
		// TODO OPT remove this sanity check if confident
		self.convert_points(lmi, time_final, points);
		Ok(())
	}
	
	/// This assumes settle_funding was already called
	pub fn change_base_position(&mut self, perp_market: &mut PerpMarket, base_change: i64) {
		let start = self.base_position;
		self.base_position += base_change;
		perp_market.open_interest += self.base_position.abs() - start.abs();
	}
	
	/// Move unrealized funding payments into the quote_position
	pub fn settle_funding(&mut self, cache: &PerpMarketCache) {
		if self.base_position > 0 {
			self.quote_position -= (cache.long_funding - self.long_settled_funding)
				  * I80F48::from_num(self.base_position);
		} else if self.base_position < 0 {
			self.quote_position -= (cache.short_funding - self.short_settled_funding)
				  * I80F48::from_num(self.base_position);
		}
		self.long_settled_funding = cache.long_funding;
		self.short_settled_funding = cache.short_funding;
	}
	
	/// Get quote position adjusted for funding
	pub fn get_quote_position(&self, pmc: &PerpMarketCache) -> I80F48 {
		if self.base_position > 0 {
			// TODO OPT use checked_fmul to not do the mul if one of these is zero
			self.quote_position
				  - (pmc.long_funding - self.long_settled_funding)
				  * I80F48::from_num(self.base_position)
		} else if self.base_position < 0 {
			self.quote_position
				  - (pmc.short_funding - self.short_settled_funding)
				  * I80F48::from_num(self.base_position)
		} else {
			self.quote_position
		}
	}
	
	/// Return (base_val, quote_val) unweighted
	pub fn get_val(
		&self,
		pmi: &PerpMarketInfo,
		pmc: &PerpMarketCache,
		price: I80F48,
	) -> MangoResult<(I80F48, I80F48)> {
		let curr_pos = self.base_position + self.taker_base;
		let bids_base_net = curr_pos.checked_add(self.bids_quantity).unwrap();
		let asks_base_net = curr_pos.checked_sub(self.asks_quantity).unwrap();
		
		if bids_base_net.checked_abs().unwrap() > asks_base_net.checked_abs().unwrap() {
			let base = I80F48::from_num(bids_base_net.checked_mul(pmi.base_lot_size).unwrap())
				  .checked_mul(price)
				  .unwrap();
			let quote = self.get_quote_position(pmc)
				  + I80F48::from_num(self.taker_quote * pmi.quote_lot_size)
				  - I80F48::from_num(self.bids_quantity.checked_mul(pmi.base_lot_size).unwrap())
				  .checked_mul(price)
				  .unwrap();
			Ok((base, quote))
		} else {
			let base = I80F48::from_num(asks_base_net.checked_mul(pmi.base_lot_size).unwrap())
				  .checked_mul(price)
				  .unwrap();
			let quote = self.get_quote_position(pmc)
				  + I80F48::from_num(self.taker_quote * pmi.quote_lot_size)
				  + I80F48::from_num(self.asks_quantity.checked_mul(pmi.base_lot_size).unwrap())
				  .checked_mul(price)
				  .unwrap();
			Ok((base, quote))
		}
	}
	
	/// Return (base_val, quote_val) unweighted after simulating effect of
	/// changes to taker_base, taker_quote, bids_quantity and asks_quantity
	pub fn sim_get_val(
		&self,
		pmi: &PerpMarketInfo,
		pmc: &PerpMarketCache,
		price: I80F48,
		taker_base: i64,
		taker_quote: i64,
		bids_quantity: i64,
		asks_quantity: i64,
	) -> MangoResult<(I80F48, I80F48)> {
		let taker_base = self.taker_base + taker_base;
		let taker_quote = self.taker_quote + taker_quote;
		let bids_quantity = self.bids_quantity.checked_add(bids_quantity).unwrap();
		let asks_quantity = self.asks_quantity.checked_add(asks_quantity).unwrap();
		
		let curr_pos = self.base_position + taker_base;
		let bids_base_net = curr_pos.checked_add(bids_quantity).unwrap();
		let asks_base_net = curr_pos.checked_sub(asks_quantity).unwrap();
		if bids_base_net.checked_abs().unwrap() > asks_base_net.checked_abs().unwrap() {
			let base = I80F48::from_num(bids_base_net.checked_mul(pmi.base_lot_size).unwrap())
				  .checked_mul(price)
				  .unwrap();
			let quote = self.get_quote_position(pmc)
				  + I80F48::from_num(taker_quote * pmi.quote_lot_size)
				  - I80F48::from_num(bids_quantity.checked_mul(pmi.base_lot_size).unwrap())
				  .checked_mul(price)
				  .unwrap();
			Ok((base, quote))
		} else {
			let base = I80F48::from_num(asks_base_net.checked_mul(pmi.base_lot_size).unwrap())
				  .checked_mul(price)
				  .unwrap();
			let quote = self.get_quote_position(pmc)
				  + I80F48::from_num(taker_quote * pmi.quote_lot_size)
				  + I80F48::from_num(asks_quantity.checked_mul(pmi.base_lot_size).unwrap())
				  .checked_mul(price)
				  .unwrap();
			Ok((base, quote))
		}
	}
	
	pub fn is_active(&self) -> bool {
		self.base_position != 0
			  || !self.quote_position.is_zero()
			  || self.bids_quantity != 0
			  || self.asks_quantity != 0
			  || self.taker_base != 0
			  || self.taker_quote != 0
		
		// Note funding only applies if base position not 0
	}
	
	/// Decrement self and increment other
	pub fn transfer_quote_position(&mut self, other: &mut PerpAccount, quantity: I80F48) {
		self.quote_position -= quantity;
		other.quote_position += quantity;
	}
	
	/// All orders must be canceled and there must be no unprocessed FillEvents for this PerpAccount
	pub fn has_no_open_orders(&self) -> bool {
		self.bids_quantity == 0
			  && self.asks_quantity == 0
			  && self.taker_quote == 0
			  && self.taker_base == 0
	}
}

#[derive(Copy, Clone, Pod)]
#[repr(C)]
/// Information regarding market maker incentives for a perp market
pub struct LiquidityMiningInfo {
	/// Used to convert liquidity points to MNGO
	pub rate: I80F48,
	
	pub max_depth_bps: I80F48, // instead of max depth bps, this should be max num contracts
	
	/// start timestamp of current liquidity incentive period; gets updated when mngo_left goes to 0
	pub period_start: u64,
	
	/// Target time length of a period in seconds
	pub target_period_length: u64,
	
	/// Paper MNGO left for this period
	pub mngo_left: u64,
	
	/// Total amount of MNGO allocated for current period
	pub mngo_per_period: u64,
}

/// This will hold top level info about the perps market
/// Likely all perps transactions on a market will be locked on this one because this will be passed in as writable
#[derive(Copy, Clone, Pod, Loadable)]
#[repr(C)]
pub struct PerpMarket {
	pub meta_data: MetaData,
	
	pub mango_group: Pubkey,
	pub bids: Pubkey,
	pub asks: Pubkey,
	pub event_queue: Pubkey,
	pub quote_lot_size: i64, // number of quote native that reresents min tick
	pub base_lot_size: i64,  // represents number of base native quantity; greater than 0
	
	// TODO - consider just moving this into the cache
	pub long_funding: I80F48,
	pub short_funding: I80F48,
	
	pub open_interest: i64, // This is i64 to keep consistent with the units of contracts, but should always be > 0
	
	pub last_updated: u64,
	pub seq_num: u64,
	pub fees_accrued: I80F48, // native quote currency
	
	pub liquidity_mining_info: LiquidityMiningInfo,
	
	// mngo_vault holds mango tokens to be disbursed as liquidity incentives for this perp market
	pub mngo_vault: Pubkey,
}

impl PerpMarket {
	
	pub fn load_checked(
		account: AccountInfo,
		program_id: &Pubkey,
		mango_group_pk: &Pubkey,
	) -> MangoResult<Self> {
		
		let state = PerpMarket::load_from_bytes(&account.data)?;
		Ok(state.clone())
	}
	
	pub fn load_mut_checked(
		account: AccountInfo,
		program_id: &Pubkey,
		mango_group_pk: &Pubkey,
	) -> MangoResult<Self> {
		let state = PerpMarket::load_from_bytes(&account.data)?;
		Ok(state.clone())
	}
	
	pub fn gen_order_id(&mut self, side: Side, price: i64) -> i128 {
		self.seq_num += 1;
		
		let upper = (price as i128) << 64;
		match side {
			Side::Bid => upper | (!self.seq_num as i128),
			Side::Ask => upper | (self.seq_num as i128),
		}
	}
	
	
	/// Convert from the price stored on the book to the price used in value calculations
	pub fn lot_to_native_price(&self, price: i64) -> I80F48 {
		I80F48::from_num(price)
			  .checked_mul(I80F48::from_num(self.quote_lot_size))
			  .unwrap()
			  .checked_div(I80F48::from_num(self.base_lot_size))
			  .unwrap()
	}
	pub fn lotToNativePriceQuantity(&self, price: u64, quantity: u64) -> (i64, i64) {
	let nativePrice = (price * self.base_lot_size as u64) / self.quote_lot_size as u64;
	let nativeQuantity = quantity / self.base_lot_size as u64;
	return (nativePrice as i64, nativeQuantity as i64);
	}
	
	/// Socialize the loss in this account across all longs and shorts
	pub fn socialize_loss(
		&mut self,
		account: &mut PerpAccount,
		cache: &mut PerpMarketCache,
	) -> MangoResult<I80F48> {
		// TODO convert into only socializing on one side
		// native USDC per contract open interest
		let socialized_loss = if self.open_interest == 0 {
			// This is kind of an unfortunate situation. This means socialized loss occurs on the
			// last person to call settle_pnl on their profits. Any advice on better mechanism
			// would be appreciated. Luckily, this will be an extremely rare situation.
			ZERO_I80F48
		} else {
			account
				  .quote_position
				  .checked_div(I80F48::from_num(self.open_interest))
				  .unwrap()
		};
		account.quote_position = ZERO_I80F48;
		self.long_funding -= socialized_loss;
		self.short_funding += socialized_loss;
		
		cache.short_funding = self.short_funding;
		cache.long_funding = self.long_funding;
		Ok(socialized_loss)
	}
}





/// Copied over from serum dex
#[derive(Copy, Clone)]
#[repr(packed)]
pub struct OrderBookStateHeader {
	pub account_flags: u64, // Initialized, (Bids or Asks)
}
unsafe impl bytemuck::Zeroable for OrderBookStateHeader {}
unsafe impl bytemuck::Pod for OrderBookStateHeader {}

/// Quantity in lamports for the agent who triggers the AdvancedOrder
pub const ADVANCED_ORDER_FEE: u64 = 500_000;

#[repr(u8)]
#[derive(Copy, Clone, PartialEq, IntoPrimitive, TryFromPrimitive)]
pub enum AdvancedOrderType {
	PerpTrigger,
	SpotTrigger, // Not implemented yet
}
#[derive(
Eq, PartialEq, Copy, Clone, TryFromPrimitive, IntoPrimitive, Serialize, Deserialize, Debug,
)]
#[repr(u8)]
#[serde(into = "u8", try_from = "u8")]
pub enum TriggerCondition {
	Above,
	Below,
}

pub const MAX_ADVANCED_ORDERS: usize = 32;


/// Store the referrer's mango account
#[derive(Copy, Clone, Pod, Loadable)]
#[repr(C)]
pub struct ReferrerMemory {
	pub meta_data: MetaData,
	pub referrer_mango_account: Pubkey,
}

impl ReferrerMemory {
	pub fn load_mut_checked(
		account: AccountInfo,
		program_id: &Pubkey,
	) -> MangoResult<Self> {
		// not really necessary because this is a PDA
		
		let state: &Self = Self::load_from_bytes(&account.data)?;
		
		
		Ok(state.clone())
	}
}

/// Register the referrer's id to be used in the URL
#[derive(Copy, Clone, Pod, Loadable)]
#[repr(C)]
pub struct ReferrerIdRecord {
	pub meta_data: MetaData,
	pub referrer_mango_account: Pubkey,
	pub id: [u8; INFO_LEN], // this id is one of the seeds
}

impl ReferrerIdRecord {

}