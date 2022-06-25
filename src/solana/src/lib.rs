
use std::error::Error;
use std::ops::Deref;
use solana_program::program_pack::Pack;
use solana_program::pubkey::Pubkey;
use solana_sdk::account::Account;
use serde::{Serialize, Deserialize};
use mangol_common::errors::{MangolResult, SolanaError};
pub mod connection;
pub struct TokenMint {
	pub decimals: u8,
	pub address: Pubkey,
	pub symbol: String,
}

pub struct TokenAccount {

}
#[derive(Deserialize, Debug)]
pub struct Token {
	chainId: u64,
	address: String,
	symbol: String,
	name: String,
	decimals: u8,
	logoURI: String
}

impl TokenMint {
	pub fn from_pubkey<'a>(account: &Pubkey) -> MangolResult<TokenMint> {
		
		let resp = reqwest::blocking::get("https://cache.jup.ag/tokens").unwrap()
		                                                                .json::<Vec<Token>>().unwrap();
		let token = resp.into_iter().find(|t| t.address == account.to_string());
		if let Some(token_mint) = token {
			Ok(TokenMint {
				decimals: token_mint.decimals,
				address: account.clone(),
				symbol: token_mint.symbol
			})
		} else {
			Err(SolanaError::TokenMintNotFound.into())
		}
		
	}
}


#[cfg(test)]
mod test {
	use std::str::FromStr;
	use solana_client::rpc_client::RpcClient;
	use solana_program::pubkey::Pubkey;
	use crate::solana::TokenMint;
	
	#[test]
	fn get_token_info_from_pubkey() {
		let sol_mint = Pubkey::from_str("So11111111111111111111111111111111111111112").unwrap();
		let decoded = TokenMint::from_pubkey(&sol_mint).unwrap();
		assert_eq!(decoded.address, sol_mint);
		assert_eq!(decoded.symbol, "SOL");
		assert_eq!(decoded.decimals, 9 as u8);
	}
}
