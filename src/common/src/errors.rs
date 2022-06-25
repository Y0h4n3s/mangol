use solana_client::client_error::{ClientError, ClientErrorKind};
use thiserror::Error;

pub type MangolResult<T> = Result<T, MangolError>;

#[derive(Error, Debug)]
pub enum MangolError {
	#[error("Solana Error")]
	SolanaError(#[from] SolanaError),
	#[error("Swap Service Error")]
	SwapServiceError(#[from] SwapServiceError)
}
#[derive(Error, Debug)]
pub enum SolanaError {
	#[error("Token mint not found")]
	TokenMintNotFound,
	#[error("RpcClient error {0}")]
	RpcClientError(ClientErrorKind),
	#[error("No program accounts exist that match the specified config")]
	ProgramAccountsNotFound,
	#[error("Failed to get status of transaction for the given commitment")]
	TransactionStatusUnknown,
	
	
}

#[derive(Error, Debug)]
pub enum SwapServiceError {
	#[error("Market {0}/{1} not found on {2}")]
	MarketNotFound(String, String, String)
}

impl From<ClientError> for MangolError {
	fn from(e: ClientError) -> Self {
			MangolError::SolanaError(SolanaError::RpcClientError(e.kind))
	}
}
