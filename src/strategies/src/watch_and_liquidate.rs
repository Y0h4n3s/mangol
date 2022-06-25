use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use std::time::Instant;

use itertools::Itertools;
use mangol_common::errors::MangolResult;
use mangol_solana::connection::SolanaConnection;
use solana_account_decoder::{UiAccountData, UiAccountEncoding};
use solana_sdk::pubkey::Pubkey;

use mangol_mango::types::{HealthCache, HealthType, load_open_orders, MangoAccount, MangoCache, MangoGroup, UserActiveAssets};

pub struct MangoLiquidator {
	pub solana_connection: Arc<SolanaConnection>,
	pub new_accounts_queue: Arc<RwLock<Vec<Arc<Pubkey>>>>,
	pub watchers: Arc<RwLock<Vec<(JoinHandle<MangolResult<()>>, Arc<Pubkey>)>>>,
}

const WS_URL: &str = "wss://ninja.genesysgo.net";

impl MangoLiquidator {
	pub fn new(solana_connection: SolanaConnection, accounts: Vec<Pubkey>) -> MangolResult<Self> {
		let my_connection = SolanaConnection::new(&solana_connection.rpc_client.url())?;
		Ok(Self {
			solana_connection: Arc::new(my_connection),
			new_accounts_queue: Arc::new(RwLock::new(accounts.iter().map(|a| Arc::new(a.clone())).collect())),
			watchers: Arc::new(RwLock::new(vec![])),
		})
	}
	
	pub fn watch_and_liquidate(&self) -> MangolResult<JoinHandle<()>> {
		let new_accounts = self.new_accounts_queue.clone();
		let watchers = self.watchers.clone();
		let connection = self.solana_connection.clone();
		Ok(std::thread::spawn(move || {
			loop {
				let mut successfully_added: Vec<Arc<Pubkey>> = vec![];
				// continuously iterate through queued accounts and watch for possible liquidation
				match new_accounts.try_read() {
					Ok(accounts) => {
						if accounts.len() > 0 {
							println!("[+] Starting watchers for {} accounts", accounts.len());
						}
						
						for account in &*accounts {
							let watchers_guard = watchers.try_read().unwrap();
							
							let account_exists = watchers_guard.iter().find(|(j, a)| a.to_string().eq(&account.to_string()));
							if let Some(acc) = account_exists {
								println!("[-] Account {} already being monitored", account.to_string());
								// account already being monitored
							} else {
								std::mem::drop(watchers_guard);
								let t_account = account.clone();
								let t_connection = connection.clone();
								
								let watch_handle: JoinHandle<MangolResult<()>> = std::thread::spawn(move || {
									let mango_program = Pubkey::from_str("mv3ekLzLbnVPNxjSKvqBpU3ZeZXPQdEC3bp5MDEBG68").unwrap();
									let mango_mainnet_group = Pubkey::from_str("98pjRuQjK3qA6gXts96PqZT4Ze5QmnCmt3QYjhbUSPue").unwrap();
									// write account liquidation watching logic here
									let mut registered = false;
									while !registered {
										let mut sub = SolanaConnection::account_subscribe(&t_account, WS_URL);
										
										
										if let Ok((mut subscription, mut context)) = sub {
											registered = true;
											let mut errored = false;
											
											loop {
												if errored {
													let mut sub = SolanaConnection::account_subscribe(&t_account, WS_URL);
													match sub {
														Ok((s, c)) => {
															subscription = s;
															context = c;
															println!("[?] Reconnected");
															errored = false;
														}
														_ => {
															continue;
														}
													}
												}
												
												if let Ok(account_info) = context.recv() {
													match account_info.value.data {
														UiAccountData::Binary(data, encoding) => {
															println!("[?] Account changed from account {} {:?}", t_account.to_string(), encoding);
															
															if encoding == UiAccountEncoding::Base64 {
																let now = Instant::now();
																let decoded_data = base64::decode(data).unwrap();
																let decoded_mango_account = MangoAccount::load_from_vec(decoded_data).unwrap();
																println!("Took: {} ms", now.elapsed().as_millis());
																
																if !decoded_mango_account.being_liquidated {
																	continue;
																}
																
																// TODO: make this part async
																
																let mango_group_account_info = t_connection.rpc_client.get_account(&mango_mainnet_group).unwrap();
																let decoded_mango_group = MangoGroup::load_checked(mango_group_account_info, &mango_program).unwrap();
																let mango_cache_account_info = t_connection.rpc_client.get_account(&decoded_mango_group.mango_cache)?;
																let decoded_mango_cache = MangoCache::load_checked(mango_cache_account_info, &mango_program, &decoded_mango_group).unwrap();
																let user_assets = UserActiveAssets::new(&decoded_mango_group, &decoded_mango_account, vec![]);
																// println!("Assets {:?}", &user_assets);
																let mut user_health_cache = HealthCache::new(user_assets);
																let mut open_orders = vec![];
																for open_orders_pk in &decoded_mango_account.spot_open_orders {
																	if *open_orders_pk == Pubkey::default() {
																		open_orders.push(None)
																	} else {
																		let open_orders_account = t_connection.rpc_client.get_account(open_orders_pk)?;
																		open_orders.push(Some(load_open_orders(open_orders_account).unwrap()))
																	}
																}
																user_health_cache.init_vals_with_orders_vec(&decoded_mango_group, &decoded_mango_cache, &decoded_mango_account, &open_orders);
																let init_health = user_health_cache.get_health(&decoded_mango_group, HealthType::Init);
																let maint_health = user_health_cache.get_health(&decoded_mango_group, HealthType::Maint);
																let equity_health = user_health_cache.get_health(&decoded_mango_group, HealthType::Equity);
																if decoded_mango_account.being_liquidated && init_health < 0 || maint_health < 0 {
																	println!("Account Liquidatable {} Your health {} {} {}", &t_account.to_string(), init_health, maint_health, equity_health);
																	mangol_mailer::send_text_with_content(format!("Account Liquidatable {} Your health {} {} {}", &t_account.to_string(), init_health, maint_health, equity_health));
																}
															}
														}
														UiAccountData::LegacyBinary(_) => {}
														UiAccountData::Json(_) => {}
													}
												} else {
													errored = true;
													eprintln!("[-] Watcher: An error occurred while receiving reconnecting...");
												}
											}
										} else {
											println!("Failed to initiate connection for {} Retrying", t_account.to_string());
										}
									}
									Ok(())
								});
								
								let mut watchers_lock = watchers.write().unwrap();
								
								(*watchers_lock).push((watch_handle, account.clone()));
								
								// add to successful list to remove watched pubkey from new accounts queue later
								successfully_added.push(account.clone());
								// println!("[+] Started watching for liquidation on account {}", account.to_string());
							}
						}
						if accounts.len() > 0 {
							println!("[+] Started watching {} accounts", accounts.len())
						}
					}
					Err(_) => {}
				}
				
				// remove successfuly monitored pubkeys from queue
				// new_accounts read guard is dropped when we went out of scope of the match block
				let mut new_accounts_lock = new_accounts.write().unwrap();
				let remove_indexes: Vec<usize> = new_accounts_lock.iter().enumerate().map(|(i, p)| i).sorted().rev().collect();
				for remove_index in remove_indexes {
					new_accounts_lock.remove(remove_index);
				}
			}
		}))
	}
	
	pub fn add_account(&self, account: &Pubkey) -> MangolResult<()> {
		match self.watchers.try_read() {
			Ok(guard) => {
				let account_exists = guard.iter().find(|(j, a)| a.to_string().eq(&account.to_string()));
				if let Some(acc) = account_exists {
					// account already being monitored
				} else {
					let mut write_lock = self.new_accounts_queue.write().unwrap();
					(*write_lock).push(Arc::new(account.clone()));
				}
			}
			Err(_) => {}
		}
		
		Ok(())
	}
}