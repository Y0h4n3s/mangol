use std::str::FromStr;
use solana_sdk::pubkey::Pubkey;
use mangol_mango::types::{MangoAccount, MangoCache, MangoGroup, PerpMarketData};
use mangol_solana::connection::SolanaConnection;
use mangol_common::errors::MangolResult;
use solana_sdk::signature::Keypair;
use mangol_mango::client::MangoClient;
use mangol_strategies::fib_trader::{FibStrat, PriceSide};

fn main() -> MangolResult<()> {
	
	
	/*
	Fib trader
	 */
	let mango_program = Pubkey::from_str("mv3ekLzLbnVPNxjSKvqBpU3ZeZXPQdEC3bp5MDEBG68").unwrap();
	let mango_account = Pubkey::from_str("CdYzrgPCiyopyKPPa4xpYz8DCdmeeNNkZe7CzVjmYX5S").unwrap();
	
	let mango_mainnet_group = Pubkey::from_str("98pjRuQjK3qA6gXts96PqZT4Ze5QmnCmt3QYjhbUSPue").unwrap();
	let connection = SolanaConnection::new("https://ninja.genesysgo.net").unwrap();
	let mango_account_info = connection.rpc_client.get_account(&mango_account).unwrap();
	let decoded_mango_account = MangoAccount::load_checked(mango_account_info, &mango_program).unwrap();
	let signer  = Keypair::from_base58_string(&std::fs::read_to_string("./key.txt").unwrap());
	
	let mango_group_account_info = connection.rpc_client.get_account(&mango_mainnet_group).unwrap();
	let decoded_mango_group = MangoGroup::load_checked(mango_group_account_info, &mango_program).unwrap();
	let mango_cache_account_info = connection.rpc_client.get_account(&decoded_mango_group.mango_cache)?;
	let decoded_mango_cache = MangoCache::load_checked(mango_cache_account_info, &mango_program, &decoded_mango_group).unwrap();
	let mango_client = MangoClient::new(&connection, decoded_mango_group, mango_mainnet_group, mango_account, decoded_mango_group.mango_cache.clone(), decoded_mango_account, decoded_mango_cache, mango_program, signer)?;
	let perp_markets = serde_json::from_str::<Vec<PerpMarketData>>(&std::fs::read_to_string("./files/perpMarkets.json").unwrap()).unwrap();
	let perp_market = perp_markets.get(3).unwrap();
	let mut fib_trader = FibStrat::new(10, 13, mango_client, PriceSide::Buy, perp_market.clone())?;
	
	fib_trader.init_position()?;
	fib_trader.start_trading()?;

	/*
	Liquidator
	 */
	
	// let connection = SolanaConnection::new("http://147.75.81.175:8899").unwrap();
	// let mango_program = Pubkey::from_str("mv3ekLzLbnVPNxjSKvqBpU3ZeZXPQdEC3bp5MDEBG68").unwrap();
	// let mango_mainnet_group = Pubkey::from_str("98pjRuQjK3qA6gXts96PqZT4Ze5QmnCmt3QYjhbUSPue").unwrap();
	// let mango_account = Pubkey::from_str("BD9cJ18XoohKz48RS5pc6TWAcsm8Uk5nEtUiAQh8YQbz").unwrap();
	// let all_mango_accounts_filters = RpcProgramAccountsConfig {
	// 	filters: Some(vec![
	// 		RpcFilterType::DataSize(4296)
	// 	]),
	// 	account_config: RpcAccountInfoConfig {
	// 		encoding: Some(UiAccountEncoding::Base64),
	// 		data_slice: None,
	// 		commitment: Some(CommitmentConfig::finalized()),
	// 		min_context_slot: None
	// 	},
	// 	with_context: None
	// };
	// let cached_mango_accounts: Vec<String>= serde_json::from_str(&std::fs::read_to_string("/home/y0h4n3s/dev/source/tests-node/mangoAccounts.json").unwrap()).unwrap();
	// let cached_mango_accounts_pks: Vec<Pubkey> = cached_mango_accounts.into_iter().map(|pk | Pubkey::from_str(&pk).unwrap()).collect();
	// let liquidator = MangoLiquidator::new(connection, cached_mango_accounts_pks)?;
	//
	// liquidator.watch_and_liquidate()?.join();
	//
	/*
		Account Watcher
	 */
// 	let connection = SolanaConnection::new("https://ninja.genesysgo.net").unwrap();
// 	let mango_traders = vec![
// 		"2XvNEzgaboHpG3g1v8rxoQ92eGxHygdShUwx4Z9W7oax",
// //"4rm5QCgFPm4d37MCawNypngV4qPWv4D5tw57KE2qUcLE",
// "8L3Hysad7Ss7tdKZ1MBqbqfE3uAPUaWPtamD7UKbvsDQ",
// "AkYeCdjYsUG7CUYRxmCWHaqeEfJ96gJQsLANhPVgjHPw",
// "8L3Hysad7Ss7tdKZ1MBqbqfE3uAPUaWPtamD7UKbvsDQ",
// "66g7RM67Y5sHrpE5NQCAVLwPaiATHuVEi4xMHaV7Wa4B",
// 		"958v4tZZCTiGE7kqaGT2PpRLsMWZWLHcYnhKuQ4LQtVF",
// 		"FjMtun22344M2oxXXZN7ZVJABC6dJ39uvd3idqH5SdCq",
// 		"DMnWGozFuqYNWZzT9T6vnWAajQNyVYdadBPQwXWuzQX3",
// 		"H6R2zNZMmhGoXLMGweGPP4Q9RtZ6RprVu7Hc868pJVbp",
// 		"4qgrNU7MXUDFbP8HArkkc2y44AJUPbSvqXw5WmCNuz36",
//
// 	];
// 	let mut join_handles: Vec<JoinHandle<()>> = vec![];
// 	for trader in &mango_traders {
// 		let watcher = mangol_strategies::watch_mango_traders::TraderWatcher::new(Pubkey::from_str(trader).unwrap(), &connection);
// 		join_handles.push(watcher.start_watch());
// 	}
// 	for join_handle in join_handles {
// 		join_handle.join();
// 	}
//
	
	
	// let mango_program = Pubkey::from_str("mv3ekLzLbnVPNxjSKvqBpU3ZeZXPQdEC3bp5MDEBG68").unwrap();
	// let mango_mainnet_group = Pubkey::from_str("98pjRuQjK3qA6gXts96PqZT4Ze5QmnCmt3QYjhbUSPue").unwrap();
	// let mango_account = Pubkey::from_str("BD9cJ18XoohKz48RS5pc6TWAcsm8Uk5nEtUiAQh8YQbz").unwrap();
	// let all_mango_accounts_filters = RpcProgramAccountsConfig {
	// 	filters: Some(vec![
	// 		RpcFilterType::DataSize(4296)
	// 	]),
	// 	account_config: RpcAccountInfoConfig {
	// 		encoding: Some(UiAccountEncoding::Base64),
	// 		data_slice: None,
	// 		commitment: Some(CommitmentConfig::finalized()),
	// 		min_context_slot: None
	// 	},
	// 	with_context: None
	// };
	// let cached_mango_accounts: Vec<String>= serde_json::from_str(&std::fs::read_to_string("/home/y0h4n3s/dev/source/tests-node/mangoAccounts.json").unwrap()).unwrap();
	// let mango_group_account_info = connection.rpc_client.get_account(&mango_mainnet_group).unwrap();
	// let decoded_mango_group = MangoGroup::load_checked(mango_group_account_info, &mango_program).unwrap();
	//
	// let account_info = connection.rpc_client.get_account(&mango_account).unwrap();
	// let decoded_mango_account = MangoAccount::load_checked(account_info, &mango_program).unwrap();
	//
	// let user_assets = UserActiveAssets::new(&decoded_mango_group, &decoded_mango_account, vec![]);
	// println!("Assets {:?}", &user_assets);
	// let mut user_health_cache = HealthCache::new(user_assets);
	// println!("Your health {}", user_health_cache.get_health(&decoded_mango_group, HealthType::Init));
	// let mut mango_accounts: Vec<MangoAccount> = vec![];
	// for account in &cached_mango_accounts {
	// 	let account_pubkey = Pubkey::from_str(account).unwrap();
	// 	let account_info = connection.rpc_client.get_account(&account_pubkey).unwrap();
	// 	let decoded_mango_account = MangoAccount::load_checked(account_info, &mango_program).unwrap();
	//
	// 	mango_accounts.push(decoded_mango_account.clone());
	// 	println!("Done, {} ", account);
	// 	if (&decoded_mango_account.orders).iter().any(| account | *account != 0 as i128) {
	// 		println!("{:?}", decoded_mango_account.orders);
	// 	}
	// }
	// println!("{}", mango_accounts.len())
	Ok(())
}
