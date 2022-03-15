use anchor_lang::AccountDeserialize;
use clap::Parser;
use clearing_house::{
    controller::funding::settle_funding_payment,
    math::margin::{calculate_liquidation_status, LiquidationType},
    state::{
        history::funding_payment::FundingPaymentHistory,
        market::Markets,
        state::State,
        user::{User, UserPositions},
    },
};
use log::{debug, info};
use rayon::iter::{IntoParallelRefMutIterator, ParallelIterator, IntoParallelRefIterator};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{account::Account, account_info::{IntoAccountInfo}};
use solana_sdk::{
    commitment_config::CommitmentConfig,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::Transaction,
};
use std::{
    cell::RefCell,
    collections::HashMap,
    env,
    error::Error,
    fs::File,
    time::{Duration, Instant}, sync::{Mutex}, thread,
};

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Path to Solana private key to use with liquidator
    #[clap(short, long)]
    keypath: String,

    /// RPC endpoint to query
    #[clap(short, long, default_value = "https://ssc-dao.genesysgo.net")]
    endpoint: String,

    /// Enable verbose logging
    #[clap(short, long)]
    verbose: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();

    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", if args.verbose { "debug" } else { "info" })
    }
    env_logger::init();

    debug!("clearing house id: {:?}", clearing_house::ID);

    let timeout = Duration::from_secs(45);
    let commitment_config = CommitmentConfig::processed();
    let client =
        RpcClient::new_with_timeout_and_commitment(args.endpoint, timeout, commitment_config);

    // fee payer and transaction signer keypair
    let payer: Keypair =
        solana_sdk::signer::keypair::read_keypair(&mut File::open(args.keypath).unwrap()).unwrap();
    info!("liquidator pubkey {}", payer.pubkey());

    let now = Instant::now();

    let mut users = Vec::<(Pubkey, User)>::new();
    let mut liquidator_drift_account = Pubkey::default();
    let mut markets = (Pubkey::default(), Markets::default());
    let mut state = (Pubkey::default(), State::default());

    let all_accounts = client.get_program_accounts(&clearing_house::id()).unwrap();

    for account in &all_accounts {
        // try deserializing into a user account
        if let Ok(user_account) = User::try_deserialize(&mut &*account.1.data) {
            if user_account.authority == payer.pubkey() {
                assert!(liquidator_drift_account == Pubkey::default());
                liquidator_drift_account = account.0;
                debug!(
                    "got: drift account {}",
                    bs58::encode(account.0.to_bytes()).into_string()
                );
            }
            users.push((account.0, user_account));
        } else if let Ok(markets_account) = Markets::try_deserialize(&mut &*account.1.data) {
            assert!(markets.0 == Pubkey::default());
            markets = (account.0, markets_account);
        } else if let Ok(state_account) = State::try_deserialize(&mut &*account.1.data) {
            assert!(state.0 == Pubkey::default());
            state = (account.0, state_account);
        }
    }

    debug!("state: {:?}", state.0);
    debug!("markets: {:?}", markets.0);

    assert!(liquidator_drift_account != Pubkey::default());
    assert!(state.0 != Pubkey::default());
    assert!(markets.0 != Pubkey::default());

    let elapsed = now.elapsed();
    info!(
        "loaded {} user accounts from {} accounts in {:?}",
        users.len(),
        all_accounts.len(),
        elapsed
    );

    let mut slot = client.get_slot()?;
    loop {
        while client.get_slot()? == slot {thread::sleep(Duration::from_millis(10))}
        slot = client.get_slot()?;

        let start = Instant::now();

        let mut data_map: HashMap<Pubkey, Account> = HashMap::new();

        let account_data: Vec<(Pubkey, Account)> =
            client.get_program_accounts(&clearing_house::id()).unwrap();
        for (pubkey, account) in account_data.into_iter() {
            assert!(!data_map.contains_key(&pubkey));
            data_map.insert(pubkey, account);
        }

        // reload markets and funding payment history and oracles
        markets = (
            markets.0,
            Markets::try_deserialize(&mut &*client.get_account_data(&markets.0).unwrap()).unwrap(),
        );

        let funding_payment_history_data =
            client.get_account_data(&state.1.funding_payment_history)?;
        
        let oracle_accounts = Mutex::new(vec![]);
        markets.1.markets.par_iter().for_each(|market| {
            oracle_accounts.lock().unwrap().push((market.amm.oracle, client.get_account(&market.amm.oracle).unwrap()));
        });
        let oracle_accounts = oracle_accounts.into_inner().unwrap();

        // loop over all users
        let min_margin = users
            .par_iter_mut()
            .filter_map(|mut user| -> Option<u128> {
                // place holder account info
                let mut oracles = vec![];
                let mut cloned_oracle_accounts = oracle_accounts.clone();
                for oracle_account in cloned_oracle_accounts.iter_mut() {
                    oracles.push(oracle_account.into_account_info());
                }

                let funding_payment_history = RefCell::new(
                    FundingPaymentHistory::try_deserialize(
                        &mut &*funding_payment_history_data.clone(),
                    )
                    .unwrap(),
                );
                let (user_positions_data, user_account_data) = (
                    data_map.get(&user.1.positions)?.data.clone(),
                    data_map.get(&user.0)?.data.clone(),
                );
                let user_positions = RefCell::new(
                    UserPositions::try_deserialize(&mut &*user_positions_data).unwrap(),
                );
                let markets = RefCell::new(markets.1);

                user.1 = User::try_deserialize(&mut &*user_account_data).unwrap();

                // Settle user's funding payments so that collateral is up to date
                settle_funding_payment(
                    &mut user.1,
                    &mut user_positions.borrow_mut(),
                    &markets.borrow(),
                    &mut funding_payment_history.borrow_mut(),
                    0,
                )
                .unwrap();

                // Verify that the user is in liquidation territory
                let liquidation_status = calculate_liquidation_status(
                    &user.1,
                    &user_positions.borrow_mut(),
                    &markets.borrow(),
                    &oracles,
                    &state.1.oracle_guard_rails,
                    slot
                )
                .unwrap();

                // is liquidatable
                if liquidation_status.liquidation_type != LiquidationType::NONE {
                    let mut accounts = vec![
                        AccountMeta::new_readonly(state.0, false),
                        AccountMeta::new_readonly(payer.pubkey(), true),
                        AccountMeta::new(liquidator_drift_account, false),
                        AccountMeta::new(user.0, false),
                        AccountMeta::new(state.1.collateral_vault, false),
                        AccountMeta::new_readonly(state.1.collateral_vault_authority, false),
                        AccountMeta::new(state.1.insurance_vault, false),
                        AccountMeta::new_readonly(state.1.insurance_vault_authority, false),
                        AccountMeta::new_readonly(spl_token::id(), false),
                        AccountMeta::new(state.1.markets, false),
                        AccountMeta::new(user.1.positions, false),
                        AccountMeta::new(state.1.trade_history, false),
                        AccountMeta::new(state.1.liquidation_history, false),
                        AccountMeta::new(state.1.funding_payment_history, false),
                    ];

                    for position in user_positions.borrow().positions {
                        if position.base_asset_amount != 0 {
                            let market = markets.borrow().markets[position.market_index as usize];
                            accounts.push(AccountMeta::new_readonly(market.amm.oracle, false));
                        }
                    }

                    let liquidate_instruction = Instruction {
                        program_id: clearing_house::id(),
                        accounts,
                        data: hex::decode("dfb3e27d302e274a").unwrap(),
                    };

                    let liquidate_transaction = Transaction::new_signed_with_payer(
                        &*vec![liquidate_instruction],
                        Some(&payer.pubkey()),
                        &vec![&payer],
                        client.get_latest_blockhash().unwrap(),
                    );
                    info!("liquidating: {:?}", user.0,);
                    info!(
                        "result: {:?}",
                        client.send_transaction(&liquidate_transaction)
                    );

                    user.1 =
                        User::try_deserialize(&mut &*client.get_account_data(&user.0).unwrap())
                            .unwrap();
                }

                Some(liquidation_status.margin_ratio)
            })
            .min();
        if let Some(min_margin) = min_margin {
            debug!("min margin: {:?}", min_margin);
        }
        info!("loaded slot {:?} in {:?}", slot, start.elapsed());
    }
}
