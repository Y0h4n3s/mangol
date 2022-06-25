use mangol_solana::connection::SolanaConnection;
use solana_account_decoder::{UiAccountData, UiAccountEncoding};
use solana_client::pubsub_client::{AccountSubscription, PubsubClientError};
use solana_client::rpc_config::RpcAccountInfoConfig;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use mangol_mango::types::MangoAccount;

pub struct TraderWatcher {
	pub trader_account: Pubkey,
	pub state: MangoAccount,
	pub solana_connection: SolanaConnection
}

impl TraderWatcher {
	pub fn new(trader_account: Pubkey, solana_connection: &SolanaConnection ) -> Self{
		let account_info = solana_connection.rpc_client.get_account(&trader_account).unwrap();
		
		let decoded_mango_account = MangoAccount::load_checked(account_info, &trader_account).unwrap();
		
		let my_connection = SolanaConnection::new(&solana_connection.rpc_client.url()).unwrap();
		Self {
			trader_account,
			state: decoded_mango_account,
			solana_connection: my_connection
		}
	}
	
	pub fn start_watch( self) -> std::thread::JoinHandle<()>{
		let watch_thread = std::thread::spawn(move || {
			let mut registered = false;
			while !registered {
				registered = self.watch_mango_account( &self.trader_account);
			}
		});
		return watch_thread
		
		
	}
	fn watch_mango_account(&self, account: &Pubkey) -> bool {
		let ws_url = "wss://ninja.genesysgo.net";
		let mut sub = self.account_subscribe(&account, ws_url);
		
		
		if let Ok((mut subscription, mut context)) = sub {
			let mut errored = false;
			
			loop {
				if errored {
					let mut sub = self.account_subscribe(&account, ws_url);
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
							println!("[?] Account changed from account {} {:?}", account.to_string(), encoding);
							
							if encoding == UiAccountEncoding::Base64 {
								let decoded_data = base64::decode(data).unwrap();
								let decoded_mango_account = MangoAccount::load_from_vec(decoded_data).unwrap();
								println!("------->> Old {:?}", self.state.orders);
								println!("------->> New {:?}", decoded_mango_account.orders)
								//mangol_mailer::send_text_with_content(format!("Account {} Updated Something is going on there", account.clone().to_string()));
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
			println!("Failed to initiate connection for {} Retrying", account.to_string());
			return false;
		}
	}
	fn account_subscribe(&self, account: &Pubkey, ws_url: &str) -> Result<AccountSubscription, PubsubClientError> {
		return solana_client::pubsub_client::PubsubClient::account_subscribe(ws_url, account, Some(RpcAccountInfoConfig { encoding: Some(UiAccountEncoding::JsonParsed), data_slice: None, commitment: Some(CommitmentConfig::finalized()), min_context_slot: None }));
	}
	
}