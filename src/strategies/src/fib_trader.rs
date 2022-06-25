
// https://play.rust-lang.org/?version=stable&mode=debug&edition=2021&gist=12543b0d2e4aa4592bcb42ae174aaedc
use std::cmp::max;
use mangol_common::errors::MangolResult;
use mangol_solana::{Token, TokenMint};
use mangol_mango::client::MangoClient;
use mangol_mango::types::{OrderType, PerpAccount, PerpMarket, PerpMarketData, PerpMarketInfo, Side};
use num_traits::pow::Pow;
use solana_sdk::pubkey::Pubkey;
use std::time::Duration;
#[derive(Copy, Clone, Debug)]
pub enum PriceSide {
	Sell,
	Buy
}

#[derive(Copy, Clone, Debug)]
pub enum FibStratOrderState {
	Filled,
	PartiallyFilled,
	Waiting,
	Initial
}

#[derive(Clone, Debug)]
pub struct FibStratOrder {
	depth: u16,
	state: FibStratOrderState,
	price: f64,
	base_size: u64,
	tx_hash: Option<String>
	
}

#[derive(Clone, Debug)]
pub enum FibStratPositionState {
	Selling(FibStratOrder),
	Buying(FibStratOrder)
}
#[derive( Clone, Debug)]
pub struct FibStratPosition {
	pub state_history: Vec<FibStratPositionState>,
	pub current_state: FibStratPositionState,
	pub max_position_depth: u16,
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
				starting_position_size: FIB_RATIO
			},
			action_interval_secs,
			mango_client,
			starting_sentiment: sentiment,
			market,
			sentiment
		})
	}
	
	pub fn print_position(&mut self) -> MangolResult<()> {
		self.mango_client.update()?;
		let perp_account: PerpAccount = self.mango_client.mango_account.perp_accounts[self.market.market_index];
		println!("{:?}", perp_account);
		Ok(())
	}
	
	pub fn init_position(&mut self) -> MangolResult<bool> {
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
				let target_price = fib_calculator::get_price_at_n(1, oracle_price, -1)?;
				let next_quantity = self.market.ui_to_quote_units(fib_calculator::get_quantity_at_n(1, TRADE_AMOUNT)?)/ self.mango_client.mango_group.perp_markets[self.market.market_index].quote_lot_size as f64;;
				let next_order_hash = self.mango_client.place_perp_order(
					&perp_market,
					&self.market,
					Side::Bid,
					target_price,
					next_quantity.round().to_string().parse::<i64>().unwrap(),
					OrderType::Limit,
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
					false,
					None
				)?;
				order.tx_hash = Some(order_hash);
				order.price = oracle_price;
			}
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
			}
		}
		
		if position_size == 0.0 {
			Ok(self.mango_client.mango_cache.get_price(self.market.market_index))
		} else {
			Ok(position_value / position_size)
		}
	}
	
	pub fn get_position_size(&self) -> MangolResult<f64> {
		let mut position_size: f64 = 0.0;
		for past_state in &self.position.state_history {
			match past_state {
				FibStratPositionState::Selling(order) => {
					position_size = position_size - order.base_size as f64
				}
				FibStratPositionState::Buying(order) => {
					position_size = position_size + order.base_size as f64
				}
			}
		}
		Ok(position_size.abs())
		
	}
	
	pub fn reset(&mut self) -> MangolResult<()> {
		
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
				starting_position_size: FIB_RATIO
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
			trade_quantity =  self.get_quantity_lots_at_n(depth)?;
		}
		
		Ok(trade_quantity)
	}
	
	pub fn get_quantity_lots_at_n(&self, depth: u16) -> MangolResult<i64> {
		Ok((self.market.ui_to_quote_units(fib_calculator::get_quantity_at_n( depth, TRADE_AMOUNT)?)/ self.mango_client.mango_group.perp_markets[self.market.market_index].quote_lot_size as f64).round().to_string().parse::<i64>().unwrap())
	}
	
	pub fn sync_bearish(&mut self) -> MangolResult<()> {
		// sync onchain state
		let prev_perp_market_info: &PerpMarketInfo = self.mango_client.mango_group.perp_markets.get(self.market.market_index as usize).unwrap();
		let prev_perp_account: PerpAccount = self.mango_client.mango_account.perp_accounts[self.market.market_index];
		let prev_mango_cache = self.mango_client.mango_cache.clone();
		self.mango_client.update()?;
		let curr_perp_market_info: &PerpMarketInfo = self.mango_client.mango_group.perp_markets.get(self.market.market_index as usize).unwrap();
		let curr_perp_account: PerpAccount = self.mango_client.mango_account.perp_accounts[self.market.market_index];
		let curr_mango_cache = self.mango_client.mango_cache.clone();
		let curr_position_size = self.get_position_size()?;
		
		let mut previous_state = self.position.current_state.clone();
		
		match &mut previous_state {
			FibStratPositionState::Selling(order) => {
				let trade_quantity =  self.get_quantity_lots_at_n(order.depth)?;
				let native_price = curr_perp_market_info.lot_to_native_price(order.price);
				let expected_base_filled = trade_quantity / native_price;
				let actual_base_filled = (prev_perp_account.base_position - curr_perp_account.base_position).abs();
				println!("Previous state SELLING Expected to be filled: {} Actual filled: {}", expected_base_filled, actual_base_filled);
				if actual_base_filled == 0 {
					// order was not filled
				}
				else if expected_base_filled > actual_base_filled {
					// handle partially filled order here
					// save it to a partially filled list and do sth
					mangol_mailer::send_text_with_content(format!("Partial fill encountered idk what to do reset me rn {:?}", &self.position));
				}
				
				else if expected_base_filled <= actual_base_filled {
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
				else if expected_base_filled > actual_base_filled {
					// handle partially filled order here
					// save it to a partially filled list and do sth
					mangol_mailer::send_text_with_content(format!("Partial fill encountered reset me rn {:?}", &self.position));
				}
				else if expected_base_filled <= actual_base_filled {
					// order was succesful
					// meaning previous 1 sell order or previous n - RISK_TOLERANCE depth orders have been profited on and closed
					// therefore adjust depth to reflect current position size for the decision round
					order.base_size = actual_base_filled as u64;
					order.state = FibStratOrderState::Filled;
					order.depth = if order.depth > RISK_TOLERANCE { order.depth - RISK_TOLERANCE} else if order.depth >= 1 {order.depth - 1} else {0};
					
					self.position.state_history.push(previous_state.clone());
				}
			}
		}
		Ok(())
		
	}
	
	pub fn sync_bullish(&mut self) -> MangolResult<()> {
		
		Ok(())
	}
		
		pub fn decide_bearish(&mut self) -> MangolResult<()> {
		
		let mango_cache = self.mango_client.mango_cache.clone();
		let perp_market_info: &PerpMarketInfo = self.mango_client.mango_group.perp_markets.get(self.market.market_index as usize).unwrap();
		
		let average_price = self.get_average_price()?;
		let curr_position_size = self.get_position_size()?;
		if curr_position_size == 0.0 {
			// position is closed reset on next iteration
			return Ok(())
		}
		let oracle_price = mango_cache.get_price(self.market.market_index);
		println!("Using average price: {} oracle price: {} and position size: {}", average_price, oracle_price, curr_position_size);
		
		let last_committed_state = self.position.state_history.get(self.position.state_history.len() - 1).unwrap();
		println!("Last Known state: {:?}", last_committed_state);
		if oracle_price > average_price {
			match last_committed_state {
				FibStratPositionState::Selling(order) | FibStratPositionState::Buying(order) => {
					// calculate next price target and size
					let target_price = fib_calculator::get_price_at_n(order.depth + 1, average_price, 1)?;
					let next_quantity = self.get_quantity_lots_at_n(order.depth + 1)?;
					
					let next_order_hash = self.mango_client.place_perp_order(
						perp_market_info,
						&self.market,
						Side::Ask,
						target_price,
						next_quantity,
						OrderType::Limit,
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

			}
		} else {
			match last_committed_state {
				FibStratPositionState::Selling(order) | FibStratPositionState::Buying(order) => {
					
					// calculate next price target and size
					let target_price = fib_calculator::get_price_at_n(1, average_price, -1)?;
					let next_quantity = self.get_profit_size_at_n(order.depth)?;
					let next_order_hash = self.mango_client.place_perp_order(
						perp_market_info,
						&self.market,
						Side::Bid,
						target_price,
						next_quantity,
						OrderType::Limit,
						order.depth == 0,
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
			println!("Sleeping for {} secs", self.action_interval_secs);
			std::thread::sleep(Duration::from_secs(self.action_interval_secs));
			let perp_account: PerpAccount = self.mango_client.mango_account.perp_accounts[self.market.market_index];
			let curr_position_size = self.get_position_size()?;
			
			if perp_account.base_position == 0  || curr_position_size == 0.0 {
				// The position has been closed, reset
				println!("Position in neutral state, resetting... {:?}", perp_account);
				self.reset()?;
				continue;
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