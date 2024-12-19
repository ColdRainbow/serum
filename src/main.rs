use anchor_client::{
    anchor_lang::{prelude::AccountMeta, solana_program::hash},
    solana_sdk::{
        commitment_config::CommitmentConfig,
        instruction::Instruction,
        message::Message,
        pubkey::Pubkey as AnchorPubkey,
        signature::{Keypair, Signer},
        signer::{EncodableKey, SignerError},
        signers::Signers,
        system_instruction,
        transaction::Transaction,
    },
    Client, Cluster,
};
use base64::{engine::Engine, prelude::BASE64_STANDARD};
use clap::{Parser, Subcommand};
use coral_multisig::instruction as multisig_instructions;
use coral_multisig::{accounts as multisig_accounts, TransactionAccount};
use solana_remote_wallet::{
    ledger::LedgerWallet,
    locator::Manufacturer,
    remote_wallet::{RemoteWallet, RemoteWalletError},
};
use solana_sdk::derivation_path::DerivationPath;
use spl_token::instruction as token_instruction;
use std::str::FromStr;
use std::{ops::Deref, rc::Rc};

#[derive(Clone)]
struct LedgerSigner {
    ledger: Rc<LedgerWallet>,
    derivation_path: DerivationPath,
}

impl Signer for LedgerSigner {
    fn try_pubkey(&self) -> Result<AnchorPubkey, anchor_client::solana_sdk::signer::SignerError> {
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
    ) -> Result<
        anchor_client::solana_sdk::signature::Signature,
        anchor_client::solana_sdk::signer::SignerError,
    > {
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

#[derive(Parser)]
struct Cli {
    #[arg(long = "ledger", default_value_t = false)]
    ledger: bool,
    #[arg(short = 'n', long = "account-number", default_value_t = 0)]
    account_number: u32,

    #[arg(long = "rpc")]
    rpc: Option<String>,

    #[arg(short = 'k', long = "private-key")]
    key_file: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create new multisig
    CreateMultisig {
        #[arg(long = "signers")]
        signers: Vec<AnchorPubkey>,
        #[arg(long = "threshold")]
        threshold: u64,
    },
    /// Create a token transfer transaction
    CreateTransaction {
        #[arg(long = "multisig")]
        multisig: AnchorPubkey,
        #[arg(long = "from")]
        from: AnchorPubkey,
        #[arg(long = "to")]
        to: AnchorPubkey,
        #[arg(long = "amount")]
        amount: u64,
    },
    /// Approve a pending transaction
    Approve {
        #[arg(long = "multisig")]
        multisig: AnchorPubkey,
        #[arg(long = "transaction")]
        transaction: AnchorPubkey,
    },
    /// Execute an approved transaction
    ExecuteTransaction {
        #[arg(long = "multisig")]
        multisig: AnchorPubkey,
        #[arg(long = "transaction")]
        transaction: AnchorPubkey,
        #[arg(long = "from")]
        from: AnchorPubkey,
        #[arg(long = "to")]
        to: AnchorPubkey,
    },
}

fn build_tx(
    signers: impl Signers,
    blockhash: hash::Hash,
    payer: AnchorPubkey,
    instructions: Vec<Instruction>,
) -> Transaction {
    let message = Message::new_with_blockhash(&instructions, Some(&payer), &blockhash);
    println!("You may now check the transaction using external tools.\nHere is the transaction data in base64:\n{}",
        BASE64_STANDARD.encode(message.serialize())
    );
    println!("Press enter, when you're finished");

    let mut s = String::new();
    std::io::stdin().read_line(&mut s).unwrap();

    println!("Now the transaction must be signed");
    Transaction::new_signed_with_payer(&instructions, Some(&payer), &signers, blockhash)
}

async fn run<S: Clone + Deref<Target = impl Signer>>(signer: S, cli: Cli) -> anyhow::Result<()> {
    let cluster = if let Some(ref rpc) = cli.rpc {
        rpc.parse()?
    } else {
        Cluster::Devnet
    };
    let client = Client::new_with_options(cluster, signer.clone(), CommitmentConfig::processed());

    // Program IDs
    let multisig_program_id =
        // AnchorPubkey::from_str("AAHT26ecV3FEeFmL2gDZW6FfEqjPkghHbAkNZGqwT8Ww").unwrap(); // MAINNET
        AnchorPubkey::from_str("msigUdDBsR4zSUYqYEDrc1LcgtmuSDDM7KxpRUXNC6U").unwrap(); // DEVNET

    // Program instance
    let program = client.program(multisig_program_id)?;

    match cli.command {
        Command::CreateMultisig { signers, threshold } => {
            let keypair = Keypair::new();
            let accounts = multisig_accounts::CreateMultisig {
                multisig: keypair.pubkey(),
            };
            let (multisig_pda, nonce) =
                derive_multisig_signer(&keypair.pubkey(), &multisig_program_id);
            let instructions = multisig_instructions::CreateMultisig {
                owners: signers,
                threshold,
                nonce,
            };
            let sig = program
                .request()
                .accounts(accounts)
                .args(instructions)
                .instruction(system_instruction::create_account(
                    &program.payer(),
                    &keypair.pubkey(),
                    program.rpc().get_minimum_balance_for_rent_exemption(500)?,
                    500,
                    &program.id(),
                ))
                .signer(&signer)
                .signer(&keypair)
                .send()
                .await?;

            println!("Transaction created: {}", sig);
            println!("Multisig address: {}", keypair.pubkey());
            println!("Multisig PDA: {}", multisig_pda);
        }
        Command::CreateTransaction {
            multisig,
            from,
            to,
            amount,
        } => {
            let keypair = Keypair::new();
            let (multisig_pda, _) = derive_multisig_signer(&multisig, &multisig_program_id);
            let transfer = token_instruction::transfer(
                &spl_token::id(),
                &from.to_bytes().into(),
                &to.to_bytes().into(),
                &multisig_pda.to_bytes().into(),
                &[],
                amount,
            )?;

            let transfer_accounts: Vec<TransactionAccount> =
                transfer.accounts.iter().map(Into::into).collect();
            let accounts = multisig_accounts::CreateTransaction {
                multisig,
                transaction: keypair.pubkey(),
                proposer: signer.pubkey(),
            };
            let instructions = multisig_instructions::CreateTransaction {
                pid: spl_token::id().to_bytes().into(),
                accs: transfer_accounts,
                data: transfer.data,
            };
            let req = program
                .request()
                .accounts(accounts)
                .args(instructions)
                .instruction(system_instruction::create_account(
                    &program.payer(),
                    &keypair.pubkey(),
                    program.rpc().get_minimum_balance_for_rent_exemption(500)?,
                    500,
                    &program.id(),
                ));

            let blockhash = program.rpc().get_latest_blockhash()?;
            let signers: Vec<&dyn Signer> = vec![&signer, &keypair, &signer];
            let tx = build_tx(signers, blockhash, signer.pubkey(), req.instructions()?);
            let sig = program
                .async_rpc()
                .send_and_confirm_transaction(&tx)
                .await?;

            println!("Transaction created: {}", sig);
            println!("Pending transaction account: {}", keypair.pubkey());
        }
        Command::Approve {
            multisig,
            transaction,
        } => {
            let accounts = multisig_accounts::Approve {
                multisig,
                transaction,
                owner: signer.pubkey(),
            };
            let instructions = multisig_instructions::Approve {};
            let req = program.request().accounts(accounts).args(instructions);

            let blockhash = program.rpc().get_latest_blockhash()?;
            let signers: Vec<&dyn Signer> = vec![&signer, &signer];
            let tx = build_tx(signers, blockhash, signer.pubkey(), req.instructions()?);
            let sig = program
                .async_rpc()
                .send_and_confirm_transaction(&tx)
                .await?;

            println!("Transaction created: {}", sig);
        }
        Command::ExecuteTransaction {
            multisig,
            transaction,
            from,
            to,
        } => {
            let (multisig_pda, _) = derive_multisig_signer(&multisig, &multisig_program_id);
            let accounts = multisig_accounts::ExecuteTransaction {
                multisig: multisig.to_bytes().into(),
                multisig_signer: multisig_pda.to_bytes().into(),
                transaction: transaction.to_bytes().into(),
            };
            let instructions = multisig_instructions::ExecuteTransaction {};
            let req = program
                .request()
                .accounts(accounts)
                .accounts(AccountMeta::new_readonly(
                    multisig_pda.to_bytes().into(),
                    false,
                ))
                .accounts(AccountMeta::new(from, false))
                .accounts(AccountMeta::new(to.to_bytes().into(), false))
                .accounts(AccountMeta::new(spl_token::id().to_bytes().into(), false))
                .args(instructions);

            let blockhash = program.rpc().get_latest_blockhash()?;
            let signers: Vec<&dyn Signer> = vec![&signer, &signer];
            let tx = build_tx(signers, blockhash, signer.pubkey(), req.instructions()?);
            let sig = program
                .async_rpc()
                .send_and_confirm_transaction(&tx)
                .await?;

            println!("Transaction created: {}", sig);
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
        run(&signer, cli).await?;
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
        )
        .await?;
    };
    Ok(())
}

fn derive_multisig_signer(
    multisig: &AnchorPubkey,
    program_id: &AnchorPubkey,
) -> (AnchorPubkey, u8) {
    AnchorPubkey::find_program_address(&[multisig.as_ref()], program_id)
}
