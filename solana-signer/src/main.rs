use std::rc::Rc;

use anchor_client::Cluster;
use base64::{prelude::BASE64_STANDARD, Engine};
use clap::Parser;
use solana_remote_wallet::{
    ledger::LedgerWallet,
    locator::Manufacturer,
    remote_wallet::{RemoteWallet, RemoteWalletError},
};
use solana_sdk::{
    derivation_path::DerivationPath,
    pubkey::Pubkey,
    signature::Keypair,
    signer::{EncodableKey, Signer, SignerError},
};

#[derive(Parser)]
struct Cli {
    #[arg(long = "ledger", default_value_t = false)]
    ledger: bool,
    #[arg(short = 'n', long = "account-number", default_value_t = 0)]
    account_number: u32,

    #[arg(long = "cluster", default_value_t = Cluster::Devnet)]
    cluster: Cluster,

    #[arg(short = 'k', long = "private-key")]
    key_file: Option<String>,

    transaction: String,
}

#[derive(Clone)]
struct LedgerSigner {
    ledger: Rc<LedgerWallet>,
    derivation_path: DerivationPath,
}

impl Signer for LedgerSigner {
    fn try_pubkey(&self) -> Result<Pubkey, solana_sdk::signer::SignerError> {
        Ok(self
            .ledger
            .get_pubkey(&self.derivation_path, false)
            .map_err(ledger_to_signer_error)?
            .to_bytes()
            .into())
    }

    fn try_sign_message(
        &self,
        message: &[u8],
    ) -> Result<solana_sdk::signature::Signature, solana_sdk::signer::SignerError> {
        Ok(self
            .ledger
            .sign_message(&self.derivation_path, message)
            .map_err(ledger_to_signer_error)?
            .as_ref()
            .try_into()
            .unwrap())
    }

    fn is_interactive(&self) -> bool {
        true
    }
}

fn ledger_to_signer_error(e: RemoteWalletError) -> SignerError {
    SignerError::Custom(e.to_string())
}

fn run(signer: impl Signer, cli: Cli) -> anyhow::Result<()> {
    let message = BASE64_STANDARD.decode(cli.transaction)?;
    let sig = signer.sign_message(&message);
    println!("Message signed:\n{}", sig);
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if cli.ledger {
        let wallet_manager = solana_remote_wallet::remote_wallet::initialize_wallet_manager()?;
        wallet_manager.update_devices()?;
        let ledger_info = wallet_manager
            .list_devices()
            .into_iter()
            .find(|wi| matches!(wi.manufacturer, Manufacturer::Ledger))
            .ok_or(anyhow::Error::msg("Ledger not found. Please, ensure that it is connected, unlocked, and the Solana app is opened"))?;
        let ledger = wallet_manager.get_ledger(&ledger_info.host_device_path)?;
        let signer = LedgerSigner {
            ledger,
            derivation_path: DerivationPath::new_bip44(Some(cli.account_number), None),
        };
        run(signer, cli)?;
    } else {
        run(
            Rc::new(
                Keypair::read_from_file(
                    cli.key_file
                        .clone()
                        .ok_or(anyhow::Error::msg("private-key is required"))?,
                )
                .unwrap(),
            ),
            cli,
        )?;
    };
    Ok(())
}
