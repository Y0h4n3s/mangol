use std::sync::Arc;
use std::time::Duration;
use solana_client::rpc_client::{GetConfirmedSignaturesForAddress2Config, RpcClient};
use solana_client::rpc_config::RpcProgramAccountsConfig;
use solana_client::tpu_client::{TpuClient, TpuClientConfig};
use solana_client::client_error::ClientErrorKind;
use solana_client::rpc_request;
use solana_program::pubkey::Pubkey;
use solana_sdk::account::Account;
use itertools::Itertools;
use solana_sdk::commitment_config::CommitmentConfig;
use mangol_common::errors::{MangolError, MangolResult, SolanaError};
use solana_client::pubsub_client::PubsubClient;
use solana_client::rpc_config::RpcAccountInfoConfig;
use solana_account_decoder::{UiAccountData, UiAccountEncoding};
use solana_client::pubsub_client::{AccountSubscription, PubsubClientError};
use solana_sdk::transaction::{Transaction, TransactionError};
use std::time::Instant;
use solana_sdk::signature::{Keypair, Signature};
use std::thread::sleep;
use solana_program::hash::hash;
use solana_program::instruction::InstructionError as IError;
use solana_sdk::transaction::TransactionError::InstructionError;

pub struct SolanaConnection {
	pub rpc_client: RpcClient,
	
	//TODO: experiment with tpu clients and sending txs to the next leader
	pub tpu_client: TpuClient,

}

impl SolanaConnection {
	pub fn new(rpc_addr: &str) -> MangolResult<Self> {
		let rpc_client = RpcClient::new_with_timeout_and_commitment(rpc_addr, Duration::from_secs(120), CommitmentConfig::confirmed());
		let tpu_client = TpuClient::new(Arc::new(RpcClient::new(rpc_addr)), "wss://ninja.genesysgo.net", TpuClientConfig { fanout_slots: 50 }).unwrap();

		Ok(Self {
			rpc_client,
			tpu_client
		})
	}
	
	pub fn get_leader(&self) -> MangolResult<bool> {
		let leaders = self.rpc_client.get_leader_schedule(None).unwrap().unwrap();
		
		println!("{:?}", leaders);
		Ok(false)
	}
	
	pub fn get_program_accounts_with_config(
		&self,
		pubkey: &Pubkey,
		config: &RpcProgramAccountsConfig
	) -> MangolResult<Vec<(Pubkey, Account)>> {
		let response = self.rpc_client.get_program_accounts_with_config(pubkey, config.clone());
		if let Ok(accounts) = response {
			Ok(accounts)
		} else {
			Err(SolanaError::RpcClientError(response.unwrap_err().kind).into())
		}
	}
	
	pub fn get_first_program_account_with_config(
		&self,
		pubkey: &Pubkey,
		config: &RpcProgramAccountsConfig
	) -> MangolResult<(Pubkey, Account)> {
		let accounts = self.get_program_accounts_with_config(pubkey, config)?;
		if accounts.len() <= 0 {
			Err(SolanaError::ProgramAccountsNotFound.into())
		} else {
			Ok((accounts[0].0.clone(), accounts[0].1.clone()))
		}
		
	}
	
	pub fn get_latest_program_account_with_config(
		&self,
		pubkey: &Pubkey,
		config: &RpcProgramAccountsConfig
	) -> MangolResult<(Pubkey, Account)> {
		let accounts = self.get_program_accounts_with_config(pubkey, config)?;
		if accounts.len() <= 0 {
			Err(SolanaError::ProgramAccountsNotFound.into())
		} else if accounts.len() == 1 {
			Ok((accounts[0].0.clone(), accounts[0].1.clone()))
		} else {
			let mut account_times = vec![];
			for (pubkey, account) in &accounts {
				let last_sigs = self.rpc_client.get_signatures_for_address_with_config(&pubkey, GetConfirmedSignaturesForAddress2Config {
					before: None,
					commitment: Some(CommitmentConfig::finalized()),
					until: None,
					limit: Some(1)
				}).unwrap();
				if last_sigs.len() <= 0 {
					account_times.push(0);
					continue
				}
				account_times.push(last_sigs.get(0).unwrap().slot)
			}
			
			let latest_slot_index = account_times.into_iter().position_max().unwrap();
			Ok((accounts[latest_slot_index].0.clone(), accounts[latest_slot_index].1.clone()))
			
		}
		
	}
	pub fn account_subscribe(account: &Pubkey, ws_url: &str) -> Result<AccountSubscription, PubsubClientError> {
		return solana_client::pubsub_client::PubsubClient::account_subscribe(ws_url, account, Some(RpcAccountInfoConfig { encoding: Some(UiAccountEncoding::JsonParsed), data_slice: None, commitment: Some(CommitmentConfig::finalized()), min_context_slot: None }));
	}
	
	pub fn try_tx_once(&self, transaction: Transaction, signer: &Keypair) -> MangolResult<String> {
		const SEND_RETRIES: usize = 15;
		const GET_STATUS_RETRIES: usize = 155;
		let now = Instant::now();
		let recent_blockhash = self.rpc_client.get_latest_blockhash().unwrap();
		
		let mut signed_transaction = transaction.clone();
		signed_transaction.sign(&[signer], recent_blockhash);
		'sending: for _ in 0..SEND_RETRIES {
			let sig = self.rpc_client.send_transaction(&signed_transaction);
			if let Ok(signature) = sig {
				
				
				'confirmation: for status_retry in 0..usize::MAX {
					let result: Result<Signature, Option<TransactionError>> =
						  match self.rpc_client.get_signature_status_with_commitment(&signature,CommitmentConfig::finalized()) {
							  Ok(res) => {
								  match res {
									  Some(Ok(_)) => Ok(signature),
									  Some(Err(e)) => Err(Some(e.into())),
									  None => {
										  if status_retry < GET_STATUS_RETRIES
										  {
											  // Retry in a second
											  sleep(Duration::from_millis(1000));
											  Err(None)
										  } else {
											  println!("[?] Transaction not finalized in {} seconds resending", now.elapsed().as_secs());
											  break 'confirmation;
										  }
									  }
								  }
								 
							  }
							  Err(e) => {
								  eprintln!("{:?}", e);
								  // Retry in a second
								  sleep(Duration::from_millis(1000));
								  Err(None)
							  }
							  
						  };
					match result {
						Ok(signature) => {
								println!("[+] Transaction Successful: {:?}", sig);
								return Ok(sig.unwrap().to_string())
							
						}
						Err(None) => {
							//eprintln!("[-] Failed to finalize transaction {} retrying...", signature);
							continue
						}
						Err (e) => {
							match e.as_ref().unwrap() {
								TransactionError::InstructionError(0, err) => {
									if !err.eq(&IError::Custom(33)) {
										eprintln!("[-] Transaction Failed: {:?}", err );
										return Err(MangolError::SolanaError(SolanaError::ProgramAccountsNotFound))
									} else {
										continue
									}
								}
								_ => {
									eprintln!("[-] Transaction Failed: {:?}", e );
									return Err(MangolError::SolanaError(SolanaError::ProgramAccountsNotFound))
								}
							}
							
						}
					}
				}
			} else {
				let err = sig.unwrap_err();
				eprintln!("[-] An Error Occurred While sending tx: {:?}", &err );
				match &err.kind {
					ClientErrorKind::RpcError(e) => {
						match e {
							rpc_request::RpcError::RpcResponseError {
								code,
								..
							} => {
								if *code == -32002 {
									// update blockhash
									let recent_blockhash = self.rpc_client.get_latest_blockhash().unwrap();
									signed_transaction = transaction.clone();
									signed_transaction.sign(&[signer], recent_blockhash);
									
								}
							}
							_ => {
							
							}
						}
					}
					_ => {
					
					}
				}
				continue
			}
		}
		Ok("".to_string())
		
	}
	
	
	
	
}