use anchor_client::{
    anchor_lang::{prelude::AccountMeta, solana_program::hash},
    solana_sdk::{
        commitment_config::CommitmentConfig,
        instruction::Instruction,
        message::Message,
        pubkey::Pubkey as AnchorPubkey,
        signature::{Keypair, Signature, Signer},
        system_instruction, sysvar,
        transaction::Transaction,
    },
    Client, Cluster,
};
use base64::{engine::Engine, prelude::BASE64_STANDARD};
use clap::{Args, Parser, Subcommand};
use coral_multisig::instruction as multisig_instructions;
use coral_multisig::{accounts as multisig_accounts, TransactionAccount};
use crossterm::style::{style, Stylize};
use spl_token::instruction::{self as token_instruction, TokenInstruction};

#[derive(Parser)]
struct Cli {
    #[arg(
        long = "pid",
        default_value = "AAHT26ecV3FEeFmL2gDZW6FfEqjPkghHbAkNZGqwT8Ww" // Devnet: msigUdDBsR4zSUYqYEDrc1LcgtmuSDDM7KxpRUXNC6U
    )]
    pid: AnchorPubkey,
    #[arg(long = "cluster", default_value_t = Cluster::Devnet)]
    cluster: Cluster,

    #[arg(short = 'k', long = "private-key")]
    key_file: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct SignerArg {
    #[arg(long = "signer")]
    signer: AnchorPubkey,
    #[arg(long = "nonce-account")]
    nonce_account: AnchorPubkey,
    #[arg(long = "nonce")]
    nonce: hash::Hash,
}

#[derive(Subcommand)]
enum Command {
    /// Create new multisig
    #[command(hide(true))]
    CreateMultisig {
        #[command(flatten)]
        signer: SignerArg,
        #[arg(long = "signers")]
        signers: Vec<AnchorPubkey>,
        #[arg(long = "threshold")]
        threshold: u64,
    },
    /// Create a token transfer transaction
    CreateTokenTransferTransaction {
        #[command(flatten)]
        signer: SignerArg,
        #[arg(long = "multisig")]
        multisig: AnchorPubkey,
        #[arg(long = "from")]
        from: AnchorPubkey,
        #[arg(long = "to")]
        to: AnchorPubkey,
        #[arg(long = "amount")]
        amount: f64,
    },
    /// Approve a pending transaction
    Approve {
        #[command(flatten)]
        signer: SignerArg,
        #[arg(long = "multisig")]
        multisig: AnchorPubkey,
        #[arg(long = "transaction")]
        transaction: AnchorPubkey,
    },
    /// Execute an approved transaction
    ExecuteTokenTransferTransaction {
        #[command(flatten)]
        signer: SignerArg,
        #[arg(long = "multisig")]
        multisig: AnchorPubkey,
        #[arg(long = "transaction")]
        transaction: AnchorPubkey,
    },
    /// Submit a signed transaction
    Submit {
        #[arg(long = "transaction")]
        transaction: String,
        #[arg(long = "signatures")]
        signatures: Vec<Signature>,
    },
}

fn build_tx(
    payer: AnchorPubkey,
    nonce: hash::Hash,
    nonce_authority: AnchorPubkey,
    instructions: Vec<Instruction>,
) -> anyhow::Result<Message> {
    let mut message = Message::new_with_nonce(instructions, Some(&payer), &nonce_authority, &payer);
    message.recent_blockhash = nonce;
    println!("You may now check the transaction using external tools.\nHere is the transaction data in base64:\n\n{}\n",
        BASE64_STANDARD.encode(message.serialize())
    );

    Ok(message)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let dummy_signer = Keypair::new();
    let client =
        Client::new_with_options(cli.cluster, &dummy_signer, CommitmentConfig::processed());

    // Program instance
    let program = client.program(cli.pid)?;

    match cli.command {
        Command::CreateMultisig {
            signer,
            signers,
            threshold,
        } => {
            let keypair = Keypair::new();
            let accounts = multisig_accounts::CreateMultisig {
                multisig: keypair.pubkey(),
            };
            let (multisig_pda, nonce) = derive_multisig_signer(&keypair.pubkey(), &cli.pid);
            let instructions = multisig_instructions::CreateMultisig {
                owners: signers,
                threshold,
                nonce,
            };
            let req = program
                .request()
                .accounts(accounts)
                .accounts(AccountMeta::new_readonly(sysvar::rent::id(), false))
                .args(instructions)
                .instruction(system_instruction::create_account(
                    &signer.signer,
                    &keypair.pubkey(),
                    program.rpc().get_minimum_balance_for_rent_exemption(500)?,
                    500,
                    &program.id(),
                ));

            let tx = build_tx(
                signer.signer,
                signer.nonce,
                signer.nonce_account,
                req.instructions()?,
            )?;
            let sig = keypair.sign_message(&tx.serialize());

            println!("Transaction signed by multisig account: {}", sig);
            println!("Multisig address: {}", keypair.pubkey());
            println!("Multisig PDA: {}", multisig_pda);
        }
        Command::CreateTokenTransferTransaction {
            signer,
            multisig,
            from,
            to,
            amount,
        } => {
            println!(
                "{}",
                "Preparing a token transfer transaction with the following parameters:".bold()
            );
            println!(
                "Multisig address: {}\nFrom address: {}\nTo address: {}\nAmount: {}\n",
                style(multisig).green(),
                style(from).green(),
                style(to).green(),
                style(amount).green(),
            );

            let from_account = program
                .async_rpc()
                .get_token_account(&from)
                .await?
                .ok_or(anyhow::Error::msg("source token account not found"))?;
            let to_account = program
                .async_rpc()
                .get_token_account(&to)
                .await?
                .ok_or(anyhow::Error::msg("destination token account not found"))?;
            if from_account.mint != to_account.mint {
                return Err(anyhow::Error::msg(
                    "source and destination accounts have different mint addresses",
                ));
            }
            if from_account.token_amount.ui_amount.is_none()
                || from_account.token_amount.ui_amount.unwrap() < amount
            {
                return Err(anyhow::Error::msg(
                    "source account doesn't have sufficient amount of token",
                ));
            }

            let amount = spl_token::ui_amount_to_amount(amount, from_account.token_amount.decimals);

            let keypair = Keypair::new();
            let (multisig_pda, _) = derive_multisig_signer(&multisig, &cli.pid);
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
                proposer: signer.signer,
            };
            let instructions = multisig_instructions::CreateTransaction {
                pid: spl_token::id().to_bytes().into(),
                accs: transfer_accounts,
                data: transfer.data,
            };
            let req = program
                .request()
                .accounts(accounts)
                .accounts(AccountMeta::new_readonly(sysvar::rent::id(), false))
                .args(instructions)
                .instruction(system_instruction::create_account(
                    &signer.signer,
                    &keypair.pubkey(),
                    program.rpc().get_minimum_balance_for_rent_exemption(500)?,
                    500,
                    &program.id(),
                ));

            let tx = build_tx(
                signer.signer,
                signer.nonce,
                signer.nonce_account,
                req.instructions()?,
            )?;
            let sig = keypair.sign_message(&tx.serialize());

            println!(
                "Transaction signed by transaction account: {}",
                style(sig).green()
            );
            println!(
                "Pending transaction account: {}",
                style(keypair.pubkey()).green()
            );
        }
        Command::Approve {
            signer,
            multisig,
            transaction,
        } => {
            println!(
                "{}",
                "Approving a transaction with the following parameters:".bold()
            );
            println!(
                "Multisig address: {}\nTransaction address: {}\n",
                style(multisig).green(),
                style(transaction).green(),
            );

            let accounts = multisig_accounts::Approve {
                multisig,
                transaction,
                owner: signer.signer,
            };
            let instructions = multisig_instructions::Approve {};
            let req = program.request().accounts(accounts).args(instructions);

            build_tx(
                signer.signer,
                signer.nonce,
                signer.nonce_account,
                req.instructions()?,
            )?;
        }
        Command::ExecuteTokenTransferTransaction {
            signer,
            multisig,
            transaction,
        } => {
            let transaction_account: coral_multisig::Transaction =
                program.account(transaction).await?;
            let (multisig_pda, _) = derive_multisig_signer(&multisig, &cli.pid);
            let mut remaining_accounts: Vec<AccountMeta> = transaction_account
                .accounts
                .iter()
                .map(Into::into)
                .collect();
            for acc in remaining_accounts.iter_mut() {
                acc.is_signer = false;
            }
            let from_account = program
                .async_rpc()
                .get_token_account(&remaining_accounts[0].pubkey)
                .await?
                .ok_or(anyhow::Error::msg("source token account not found"))?;
            let amount = match TokenInstruction::unpack(&transaction_account.data)? {
                TokenInstruction::Transfer { amount } => Ok(amount),
                _ => Err(anyhow::Error::msg(
                    "transaction instruction is not transfer",
                )),
            }?;
            let amount = spl_token::amount_to_ui_amount(amount, from_account.token_amount.decimals);
            println!("Executing a token transfer transaction with the following parameters:");
            println!(
                "Multisig address: {}\nTransaction address: {}\nFrom: {}\nTo: {}\nAmount: {}\n",
                style(multisig).green(),
                style(transaction).green(),
                style(remaining_accounts[0].pubkey).green(),
                style(remaining_accounts[1].pubkey).green(),
                style(amount).green(),
            );

            let accounts = multisig_accounts::ExecuteTransaction {
                multisig,
                multisig_signer: multisig_pda,
                transaction,
            };
            let instructions = multisig_instructions::ExecuteTransaction {};
            let req = program
                .request()
                .accounts(accounts)
                .accounts(remaining_accounts)
                .accounts(AccountMeta::new(spl_token::id().to_bytes().into(), false))
                .args(instructions);

            build_tx(
                signer.signer,
                signer.nonce,
                signer.nonce_account,
                req.instructions()?,
            )?;
        }
        Command::Submit {
            transaction,
            signatures,
        } => {
            let data = BASE64_STANDARD.decode(transaction)?;
            let message: Message = bincode::deserialize(&data)?;
            let tx = Transaction {
                signatures,
                message,
            };
            let sig = program
                .async_rpc()
                .send_and_confirm_transaction(&tx)
                .await?;
            println!("Transaction submitted: {}", style(sig).green());
        }
    }
    Ok(())
}

fn derive_multisig_signer(
    multisig: &AnchorPubkey,
    program_id: &AnchorPubkey,
) -> (AnchorPubkey, u8) {
    AnchorPubkey::find_program_address(&[multisig.as_ref()], program_id)
}
