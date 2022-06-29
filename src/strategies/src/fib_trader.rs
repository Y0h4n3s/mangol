
	// https://play.rust-lang.org/?version=stable&mode=debug&edition=2021&gist=32e2e59946ca35ed2b31d2272b4f7823
	// https://play.rust-lang.org/?version=stable&mode=debug&edition=2021&gist=32e2e59946ca35ed2b31d2272b4f7823
use std::cmp::max;
use mangol_common::errors::MangolResult;
use mangol_solana::{Token, TokenMint};
use mangol_mango::client::MangoClient;
use mangol_mango::types::{OrderType, PerpAccount, PerpMarket, PerpMarketData, PerpMarketInfo, Side, MangoAccount};
use num_traits::pow::Pow;
use solana_sdk::pubkey::Pubkey;
use std::time::Duration;
use colored::Colorize;
use solana_sdk::signature::Signature;
use std::time::Instant;
use std::str::FromStr;
	use std::thread::sleep;
	use solana_transaction_status::UiTransactionEncoding;
	use solana_sdk::commitment_config::CommitmentConfig;
#[derive(Copy, Clone, Debug)]
pub enum PriceSide {
	Sell,
	Buy
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum FibStratOrderState {
	Filled,
	PartiallyFilled,
	Waiting,
	Initial
}

#[derive(Clone, Debug, PartialEq)]
pub struct FibStratOrder {
	depth: u16,
	state: FibStratOrderState,
	price: f64,
	base_size: u64,
	tx_hash: Option<String>
	
}

#[derive(Clone, Debug, PartialEq)]
pub enum FibStratPositionState {
	Selling(FibStratOrder),
	Buying(FibStratOrder),
	Neutral
}
#[derive( Clone, Debug)]
pub struct FibStratPosition {
	pub state_history: Vec<FibStratPositionState>,
	pub current_state: FibStratPositionState,
	pub max_position_depth: u16,
	pub furthest_position: u16,
	pub starting_position_size: f64,
}
pub struct FibStrat {
	pub position: FibStratPosition,
	pub action_interval_secs: u64,
	pub mango_client: MangoClient,
	pub starting_sentiment: PriceSide,
	pub market: PerpMarketData,
	pub sentiment: PriceSide
}

const FIB_RATIO: f64 = 1.618;
const PRICE_FIB_RATIO: f64 = 0.0618;
const TRADE_AMOUNT: f64 = 10.0;
const RISK_TOLERANCE: u16 = 2;
const PROFIT_PRICE_DEPTH: u16 = 6;
impl FibStrat {
	pub fn new(max_position_depth: u16, action_interval_secs: u64, mango_client: MangoClient, sentiment: PriceSide, market: PerpMarketData) -> MangolResult<Self>{
		let current_state = match sentiment {
			PriceSide::Sell => {
				FibStratPositionState::Selling(FibStratOrder {
					depth: 1,
					state: FibStratOrderState::Initial,
					price: 0.0,
					base_size: 0,
					tx_hash: None
				})
			}
			PriceSide::Buy => {
				FibStratPositionState::Buying(FibStratOrder {
					depth: 1,
					state: FibStratOrderState::Initial,
					price: 0.0,
					base_size: 0,
					tx_hash: None
				})
			}
		};
		Ok(Self {
			position: FibStratPosition {
				state_history: vec![],
				current_state,
				max_position_depth,
				starting_position_size: FIB_RATIO,
				furthest_position: 1
				
			},
			action_interval_secs,
			mango_client,
			starting_sentiment: sentiment,
			market,
			sentiment,
		})
	}
	
	pub fn print_position(&mut self) -> MangolResult<()> {
		self.mango_client.update()?;
		let perp_account: PerpAccount = self.mango_client.mango_account.perp_accounts[self.market.market_index];
		println!("{:?}", perp_account);
		Ok(())
	}
	
	pub fn init_position(&mut self) -> MangolResult<bool> {
		self.mango_client.update();
		let oracle_price = self.mango_client.mango_cache.get_price(self.market.market_index);
		let perp_market: PerpMarketInfo = self.mango_client.mango_group.perp_markets.get(self.market.market_index as usize).unwrap().clone();
		let quantity = self.market.ui_to_quote_units(fib_calculator::get_quantity_at_n(1, TRADE_AMOUNT)?) / self.mango_client.mango_group.perp_markets[self.market.market_index].quote_lot_size as f64;
		let perp_account: PerpAccount = self.mango_client.mango_account.perp_accounts[self.market.market_index];
		match &mut self.position.current_state {
			// this is initial state start with sell if sentiment is selling and buy otherwise
			FibStratPositionState::Selling(order) => {
				let order_hash = self.mango_client.place_perp_order(
					&perp_market,
					&self.market,
					Side::Ask,
					oracle_price,
					quantity.round().to_string().parse::<i64>().unwrap(),
					OrderType::Market,
					false,
					None
				)?;
				order.tx_hash = Some(order_hash);
				order.price = oracle_price;
				order.state = FibStratOrderState::Filled;
				self.mango_client.update()?;
				let perp_account_after: PerpAccount = self.mango_client.mango_account.perp_accounts[self.market.market_index];
				order.base_size = (perp_account.base_position - perp_account_after.base_position).abs() as u64;
				self.position.state_history.push(self.position.current_state.clone());
				
				// calculate next price target and size
				let target_price = fib_calculator::get_price_at_n(4, oracle_price, -1)?;
				let next_quantity = self.market.ui_to_quote_units(fib_calculator::get_quantity_at_n(1, TRADE_AMOUNT)?)/ self.mango_client.mango_group.perp_markets[self.market.market_index].quote_lot_size as f64;;
				
				let next_order_hash = self.mango_client.place_perp_order(
					&perp_market,
					&self.market,
					Side::Bid,
					target_price,
					next_quantity.round().to_string().parse::<i64>().unwrap(),
					OrderType::PostOnly,
					true,
					Some(self.action_interval_secs as u64 - 1)
				)?;
				self.position.current_state = FibStratPositionState::Buying(FibStratOrder {
					depth: 1,
					state: FibStratOrderState::Waiting,
					price: target_price,
					tx_hash: Some(next_order_hash),
					base_size: 0
				});
			}
			
			FibStratPositionState::Buying(order) => {
				let order_hash = self.mango_client.place_perp_order(
					&perp_market,
					&self.market,
					Side::Bid,
					oracle_price,
					quantity.round().to_string().parse::<i64>().unwrap(),
					OrderType::Market,
					true,
					None
				)?;
				order.tx_hash = Some(order_hash);
				order.price = oracle_price;
			}
			_ => {}
		}
		
		Ok(true)
	}
	
	pub fn get_average_price(&self) -> MangolResult<f64> {
		let mut position_value = 0.0;
		let mut position_size = 0.0;
		for past_state in &self.position.state_history {
			match past_state {
				FibStratPositionState::Selling(order) => {
					position_value -= (order.price * order.base_size as f64);
					position_size = position_size - order.base_size as f64
				}
				FibStratPositionState::Buying(order) => {
					position_value += (order.price * order.base_size as f64);
					position_size = position_size + order.base_size as f64
					
				}
				_ => {}
				
			}
		}
		
		if position_size == 0.0 {
			Ok(self.mango_client.mango_cache.get_price(self.market.market_index))
		} else {
			Ok(position_value / position_size)
		}
	}
	
	pub fn get_position_size(&self) -> MangolResult<i64> {
		let mut position_size: i64 = 0;
		for past_state in &self.position.state_history {
			match past_state {
				FibStratPositionState::Selling(order) => {
					position_size = position_size - order.base_size as i64
				}
				FibStratPositionState::Buying(order) => {
					position_size = position_size + order.base_size as i64
				}
				_ => {}
				
			}
		}
		Ok(position_size.abs())
		
	}
	
	pub fn reset(&mut self) -> MangolResult<()> {
		self.mango_client.update()?;
		let perp_account: PerpAccount = self.mango_client.mango_account.perp_accounts[self.market.market_index];
		let perp_market: PerpMarketInfo = self.mango_client.mango_group.perp_markets.get(self.market.market_index as usize).unwrap().clone();
		
		let oracle_price = self.mango_client.mango_cache.get_price(self.market.market_index);
		
		if perp_account.base_position > 0 {
			// sell and return to 0
			let order_hash = self.mango_client.place_perp_order_with_base(
				&perp_market,
				&self.market,
				Side::Ask,
				oracle_price,
				perp_account.base_position,
				OrderType::Market,
				true,
				None
			)?;
			println!("Neutralized position")
		} else if perp_account.base_position < 0 {
			// sentiment buy handle here
			let order_hash = self.mango_client.place_perp_order_with_base(
				&perp_market,
				&self.market,
				Side::Bid,
				oracle_price,
				perp_account.base_position.abs(),
				OrderType::Market,
				true,
				None
			)?;
			println!("Neutralized position")
			
		}
		// TODO: store previous position state somewhere for analysis
		let current_state = match self.sentiment {
			PriceSide::Sell => {
				FibStratPositionState::Selling(FibStratOrder {
					depth: 1,
					state: FibStratOrderState::Initial,
					price: 0.0,
					base_size: 0,
					tx_hash: None
				})
			}
			PriceSide::Buy => {
				FibStratPositionState::Buying(FibStratOrder {
					depth: 1,
					state: FibStratOrderState::Initial,
					price: 0.0,
					base_size: 0,
					tx_hash: None
				})
			}
		};
		self.position =  FibStratPosition {
				state_history: vec![],
				current_state,
				max_position_depth: self.position.max_position_depth,
				starting_position_size: FIB_RATIO,
			furthest_position: 1
			
		};
		self.init_position()?;
		
		Ok(())
	}
	
	pub fn get_profit_size_at_n(&self, depth: u16) -> MangolResult<i64> {
		let mut trade_quantity = 0;
		if depth > RISK_TOLERANCE {
			for i in 0..RISK_TOLERANCE {
				trade_quantity +=  self.get_quantity_lots_at_n(depth - i)?;
			}
			
		} else {
			trade_quantity =  self.get_quantity_lots_at_n(max(1, depth))?;
		}
		
		Ok(trade_quantity)
	}
	
	pub fn get_quantity_lots_at_n(&self, depth: u16) -> MangolResult<i64> {
		Ok((self.market.ui_to_quote_units(fib_calculator::get_quantity_at_n( depth, TRADE_AMOUNT)?)/ self.mango_client.mango_group.perp_markets[self.market.market_index].quote_lot_size as f64).round().to_string().parse::<i64>().unwrap())
	}
	
	pub fn sync_bearish(&mut self) -> MangolResult<()> {
		println!("{}", format!("\n>>>>>>> Bearish Sync <<<<<<<<").yellow());
		
		
		// sync onchain state
		let prev_perp_market_info: &PerpMarketInfo = self.mango_client.mango_group.perp_markets.get(self.market.market_index as usize).unwrap();
		let prev_perp_account: PerpAccount = self.mango_client.mango_account.perp_accounts[self.market.market_index];
		let prev_mango_cache = self.mango_client.mango_cache.clone();
		self.mango_client.update()?;
		let curr_perp_market_info: &PerpMarketInfo = self.mango_client.mango_group.perp_markets.get(self.market.market_index as usize).unwrap();
		let curr_perp_account: PerpAccount = self.mango_client.mango_account.perp_accounts[self.market.market_index];
		let curr_mango_cache = self.mango_client.mango_cache.clone();
		let curr_position_size = self.get_position_size()?;
		let oracle_price = self.mango_client.mango_cache.get_price(self.market.market_index);
		
		let mut previous_state = self.position.current_state.clone();
		
		match &mut previous_state {
			FibStratPositionState::Selling(order) => {
				let trade_quantity =  self.get_quantity_lots_at_n(order.depth)?;
				let native_price = curr_perp_market_info.lot_to_native_price(order.price);
				let expected_base_filled = trade_quantity / native_price;
				let actual_base_filled = (prev_perp_account.base_position - curr_perp_account.base_position).abs();
				println!("Previous state SELLING Expected to be filled: {} Actual filled: {}", expected_base_filled, actual_base_filled);
				println!();
				if actual_base_filled == 0 {
					// order was not filled
				}
				else if expected_base_filled > actual_base_filled   {
					// handle partially filled order here
					// save it to a partially filled list and do sth
					mangol_mailer::send_text_with_content(format!("Buying back partial fill of {}", actual_base_filled));
					let order_hash = self.mango_client.place_perp_order_with_base(
						curr_perp_market_info,
						&self.market,
						Side::Bid,
						oracle_price,
						actual_base_filled,
						OrderType::Market,
						false,
						None
					)?;
					let message = format!("Bought back {} https://explorer.solana.com/tx/{}", actual_base_filled, order_hash);
					mangol_mailer::send_text_with_content(message.clone());
					println!("{}", message);
					self.mango_client.update()?;
					
				}
				
				else if expected_base_filled <= actual_base_filled {
					if order.depth > self.position.furthest_position {
						self.position.furthest_position = order.depth
					}
					// order was succesful
					order.base_size = actual_base_filled as u64;
					order.state = FibStratOrderState::Filled;
					
					self.position.state_history.push(previous_state.clone());
				}
			}
			FibStratPositionState::Buying(order) => {
				// in bearish sentiment mode previous buying state always corresponds with orders to take profit,
				//
				let trade_quantity =  self.get_profit_size_at_n(order.depth)?;
				let native_price = curr_perp_market_info.lot_to_native_price(order.price);
				let expected_base_filled = trade_quantity / native_price;
				let actual_base_filled = (prev_perp_account.base_position - curr_perp_account.base_position).abs();
				println!("Previous state BUYING Expected to be filled: {} Actual filled: {}", expected_base_filled, actual_base_filled);
				if actual_base_filled == 0 {
					// order was not filled
				}
				else if expected_base_filled > actual_base_filled  {
					if order.depth == 1 {
						self.position.current_state = FibStratPositionState::Neutral;
						return Ok(())
					}
					// handle partially filled order here
					// save it to a partially filled list and do sth
					mangol_mailer::send_text_with_content(format!("Selling back partial fill of {}", actual_base_filled));
					let order_hash = self.mango_client.place_perp_order_with_base(
						curr_perp_market_info,
						&self.market,
						Side::Ask,
						oracle_price,
						actual_base_filled,
						OrderType::Market,
						false,
						None
					)?;
					let message = format!("Bought back {} https://explorer.solana.com/tx/{}", actual_base_filled, order_hash);
					mangol_mailer::send_text_with_content(message.clone());
					println!("{}", message);
					self.mango_client.update()?;
					
				}
				else if expected_base_filled <= actual_base_filled {
					if order.depth > self.position.furthest_position {
						self.position.furthest_position = order.depth
					}
					// order was succesful
					// meaning previous 1 sell order or previous n - RISK_TOLERANCE depth orders have been profited on and closed
					// therefore adjust depth to reflect current position size for the decision round
					order.base_size = actual_base_filled as u64;
					order.state = FibStratOrderState::Filled;
					order.depth = if order.depth > RISK_TOLERANCE { order.depth - RISK_TOLERANCE} else if order.depth > 1 {order.depth - 1} else {
						println!("Last order for position filled, setting to Neutral state");
						self.position.current_state = FibStratPositionState::Neutral;
						1
					};
					
					self.position.state_history.push(previous_state.clone());
				}
			}
			_ => {}
			
		}
		Ok(())
		
	}
	
	pub fn sync_bullish(&mut self) -> MangolResult<()> {
		
		Ok(())
	}
		
		pub fn decide_bearish(&mut self) -> MangolResult<()> {
			println!("{}", format!("\n>>>>>>> Bearish Decision <<<<<<<<").green());
		let mango_cache = self.mango_client.mango_cache.clone();
		let perp_market_info: &PerpMarketInfo = self.mango_client.mango_group.perp_markets.get(self.market.market_index as usize).unwrap();
		
		let average_price = self.get_average_price()?;
		let curr_position_size = self.get_position_size()?;
		if self.position.current_state == FibStratPositionState::Neutral {
			// position is closed reset on next iteration
			return Ok(())
		}
		let oracle_price = mango_cache.get_price(self.market.market_index);
		println!("Using average price: {} oracle price: {} and position size: {}", average_price, oracle_price, curr_position_size);
		
		let last_committed_state = self.position.state_history.get(self.position.state_history.len() - 1).unwrap();
		///println!("Last Known state: {:?}", last_committed_state);
		if oracle_price > average_price {
			match last_committed_state {
				FibStratPositionState::Selling(order) | FibStratPositionState::Buying(order) => {
					// calculate next price target and size
					let mut target_price = fib_calculator::get_price_at_n(order.depth + 1, average_price, 1)?;
					let next_quantity = self.get_quantity_lots_at_n(order.depth + 1)?;
					if target_price < oracle_price {
						target_price = fib_calculator::get_price_at_n( 1, oracle_price, 1)?;
					}
					let next_order_hash = self.mango_client.place_perp_order(
						perp_market_info,
						&self.market,
						Side::Ask,
						target_price,
						next_quantity,
						OrderType::PostOnly,
						order.depth == 0,
						Some(self.action_interval_secs as u64)
					)?;
					self.position.current_state = FibStratPositionState::Selling(FibStratOrder {
						depth: order.depth + 1,
						state: FibStratOrderState::Waiting,
						price: target_price,
						tx_hash: Some(next_order_hash),
						base_size: 0
					});
				}
				_ => {}
				
				
			}
		} else {
			match last_committed_state {
				FibStratPositionState::Selling(order) | FibStratPositionState::Buying(order) => {
					
					// calculate next price target and size
					let target_price_depth = if order.depth >= PROFIT_PRICE_DEPTH || ( order.depth == 1 && self.position.furthest_position > RISK_TOLERANCE ){
						1
					} else  {
						PROFIT_PRICE_DEPTH - order.depth
					};
					let mut target_price = fib_calculator::get_price_at_n(target_price_depth, average_price, -1)?;
					if target_price > oracle_price {
						target_price = fib_calculator::get_price_at_n(1, oracle_price, -1)?;
					}
					let next_quantity = self.get_profit_size_at_n(order.depth)?;
					let next_order_hash = self.mango_client.place_perp_order(
						perp_market_info,
						&self.market,
						Side::Bid,
						target_price,
						next_quantity,
						OrderType::PostOnly,
						order.depth == 1,
						Some(self.action_interval_secs as u64)
					)?;
					self.position.current_state = FibStratPositionState::Buying(FibStratOrder {
						depth: order.depth,
						state: FibStratOrderState::Waiting,
						price: target_price,
						tx_hash: Some(next_order_hash),
						base_size: 0
					});
				}
				_ => {}
				
			}
		}
		
		Ok(())
	}
	
	pub fn decide_bullish(&mut self) -> MangolResult<()> {
		Ok(())
	}
	pub fn start_trading(&mut self) -> MangolResult<()> {
		'trading_loop: loop {
			// sleep every iteration and make decisions after
			
			let perp_account: PerpAccount = self.mango_client.mango_account.perp_accounts[self.market.market_index];
			let curr_position_size = self.get_position_size()?;
			
			if self.position.current_state == FibStratPositionState::Neutral {
				// The position has been closed, reset
				println!("Position in neutral state, resetting... {:?} {:?}", perp_account, self.position);
				self.reset()?;
				continue;
			}
			let mut should_not_sleep = false;
			// check if order is on book and sleep
			match &self.position.current_state {
				FibStratPositionState::Selling(order) | FibStratPositionState::Buying(order) => {
					if order.tx_hash.is_some() {
						let mut fetch_tries = 10;
						while fetch_tries > 0 {
							if let Ok(order_tx) = self.mango_client.solana_connection.rpc_client.get_transaction(&Signature::from_str(&order.tx_hash.as_ref().unwrap()).unwrap(), UiTransactionEncoding::Base64 ) {
								fetch_tries = 0;
								for message in order_tx.transaction.meta.unwrap().log_messages.unwrap() {
									if message.contains("not be placed due to PostOnly") {
										should_not_sleep = true;
									}
								}
							} else {
								fetch_tries -= 1;
							}
						}
						
					}
					
					
				}
				_ => {}
			}
			
			if !should_not_sleep {
				let sleep_start = Instant::now();
				println!("Sleeping for {} secs", self.action_interval_secs);
				'sleep: loop {
					let elapsed_secs = sleep_start.elapsed().as_secs();
					if elapsed_secs > self.action_interval_secs {
						println!("Sleep time ended");
						break 'sleep
					}
					let mango_account_info_result = self.mango_client.solana_connection.rpc_client.get_account_with_commitment(&self.mango_client.mango_account_pk, CommitmentConfig::processed());
					if let Ok(mango_account_info) = mango_account_info_result {
						if mango_account_info.value.is_some() {
							let mango_account = MangoAccount::load_checked(mango_account_info.value.unwrap(), &self.mango_client.mango_program_id).unwrap();
							let perp_account = mango_account.perp_accounts[self.market.market_index];
							if perp_account.asks_quantity == 0 && perp_account.bids_quantity == 0 {
								println!("Order is filled or expired aborting sleep");
								break 'sleep;
							}
						}
						
					}
					std::thread::sleep(Duration::from_secs(1))
					
				}
				
			}
			let mut previous_state = self.position.current_state.clone();
			
			
			//TODO: extract everything into functions because all the data is on self
			
			match self.sentiment {
				PriceSide::Sell => {
					/*
				First update to correct current state.
				check if previous order was filled
				if order was partially filled update readjust list with info and continue to this decision round,
				if order was fully filled update average position price and size and continue,
				else just continue
				
			 */
					self.sync_bearish()?;
					/*
				Decision round
				Two main conditions
				1. current price is above average position price
					> If last sure state was selling place sell order and scale in on n+1 depth with n+1 size
					> if last sure state was buying place sell order and take profit on 1 depth with floor(n/2), 1 size
				2. current price is below average position price
					> if last sure state was selling place buy order and take profit on 1 depth with floor(n/2), 1 size
					> if last sure state was buying place buy order and scale in on n+1 depth with n+1 size
			 */
					self.decide_bearish()?;
				}
				
				PriceSide::Buy => {
				
				}
			}

		}

		Ok(())
	}
	
}


mod fib_calculator {
	use mangol_common::errors::MangolResult;
	use crate::fib_trader::{PRICE_FIB_RATIO, FIB_RATIO};
	pub fn get_price_at_n(n: u16, price: f64, direction: i8) -> MangolResult<f64> {
		// TODO: tweak this to find the best curve of increasing price targets
		// could be different for different markets
		let move_percent = PRICE_FIB_RATIO * FIB_RATIO.powf(n as f64);
		let price_change_increment = (price * move_percent) / 100.0;
		return if direction > 0 { Ok(price + price_change_increment)} else {Ok(price - price_change_increment)};
	}
	pub fn get_quantity_at_n(n: u16, quantity: f64) -> MangolResult<f64> {
			Ok(FIB_RATIO.powf(n as f64) * quantity)
	}
}

// chew argalech,
// mebrat kemeta amukew eruzun
// mitmita chemr
// tiliku jebena new yababi buna
// 6: 40 -  7 balew gize sitew