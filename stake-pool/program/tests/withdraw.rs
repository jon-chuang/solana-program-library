#![cfg(feature = "test-bpf")]

mod helpers;

use {
    borsh::{BorshDeserialize, BorshSerialize},
    helpers::*,
    solana_program::{
        hash::Hash,
        instruction::{AccountMeta, Instruction, InstructionError},
        pubkey::Pubkey,
        sysvar,
    },
    solana_program_test::*,
    solana_sdk::{
        signature::{Keypair, Signer},
        transaction::{Transaction, TransactionError},
        transport::TransportError,
    },
    spl_stake_pool::{
        borsh::try_from_slice_unchecked, error, id, instruction, stake_program, state,
    },
    spl_token::error::TokenError,
};

async fn setup() -> (
    BanksClient,
    Keypair,
    Hash,
    StakePoolAccounts,
    ValidatorStakeAccount,
    DepositInfo,
    u64,
) {
    let (mut banks_client, payer, recent_blockhash) = program_test().start().await;
    let stake_pool_accounts = StakePoolAccounts::new();
    stake_pool_accounts
        .initialize_stake_pool(&mut banks_client, &payer, &recent_blockhash)
        .await
        .unwrap();

    let validator_stake_account: ValidatorStakeAccount = simple_add_validator_to_pool(
        &mut banks_client,
        &payer,
        &recent_blockhash,
        &stake_pool_accounts,
    )
    .await;

    let deposit_info: DepositInfo = simple_deposit(
        &mut banks_client,
        &payer,
        &recent_blockhash,
        &stake_pool_accounts,
        &validator_stake_account,
    )
    .await;

    let tokens_to_burn = deposit_info.pool_tokens / 4;

    // Delegate tokens for burning
    delegate_tokens(
        &mut banks_client,
        &payer,
        &recent_blockhash,
        &deposit_info.user_pool_account,
        &deposit_info.user,
        &stake_pool_accounts.withdraw_authority,
        tokens_to_burn,
    )
    .await;

    (
        banks_client,
        payer,
        recent_blockhash,
        stake_pool_accounts,
        validator_stake_account,
        deposit_info,
        tokens_to_burn,
    )
}

#[tokio::test]
async fn test_stake_pool_withdraw() {
    let (
        mut banks_client,
        payer,
        recent_blockhash,
        stake_pool_accounts,
        validator_stake_account,
        deposit_info,
        tokens_to_burn,
    ) = setup().await;

    // Create stake account to withdraw to
    let user_stake_recipient = Keypair::new();
    let initial_stake_lamports = create_blank_stake_account(
        &mut banks_client,
        &payer,
        &recent_blockhash,
        &user_stake_recipient,
    )
    .await;

    // Save stake pool state before withdrawal
    let stake_pool_before =
        get_account(&mut banks_client, &stake_pool_accounts.stake_pool.pubkey()).await;
    let stake_pool_before =
        state::StakePool::try_from_slice(&stake_pool_before.data.as_slice()).unwrap();

    // Save validator stake account record before withdrawal
    let validator_list = get_account(
        &mut banks_client,
        &stake_pool_accounts.validator_list.pubkey(),
    )
    .await;
    let validator_list =
        try_from_slice_unchecked::<state::ValidatorList>(validator_list.data.as_slice()).unwrap();
    let validator_stake_item_before = validator_list
        .find(&validator_stake_account.vote.pubkey())
        .unwrap();

    // Save user token balance
    let user_token_balance_before =
        get_token_balance(&mut banks_client, &deposit_info.user_pool_account).await;

    let new_authority = Pubkey::new_unique();
    stake_pool_accounts
        .withdraw_stake(
            &mut banks_client,
            &payer,
            &recent_blockhash,
            &user_stake_recipient.pubkey(),
            &deposit_info.user_pool_account,
            &validator_stake_account.stake_account,
            &new_authority,
            tokens_to_burn,
        )
        .await
        .unwrap();

    // Check pool stats
    let stake_pool = get_account(&mut banks_client, &stake_pool_accounts.stake_pool.pubkey()).await;
    let stake_pool = state::StakePool::try_from_slice(&stake_pool.data.as_slice()).unwrap();
    assert_eq!(
        stake_pool.total_stake_lamports,
        stake_pool_before.total_stake_lamports - tokens_to_burn
    );
    assert_eq!(
        stake_pool.pool_token_supply,
        stake_pool_before.pool_token_supply - tokens_to_burn
    );

    // Check validator stake list storage
    let validator_list = get_account(
        &mut banks_client,
        &stake_pool_accounts.validator_list.pubkey(),
    )
    .await;
    let validator_list =
        try_from_slice_unchecked::<state::ValidatorList>(validator_list.data.as_slice()).unwrap();
    let validator_stake_item = validator_list
        .find(&validator_stake_account.vote.pubkey())
        .unwrap();
    assert_eq!(
        validator_stake_item.stake_lamports,
        validator_stake_item_before.stake_lamports - tokens_to_burn
    );

    // Check tokens burned
    let user_token_balance =
        get_token_balance(&mut banks_client, &deposit_info.user_pool_account).await;
    assert_eq!(
        user_token_balance,
        user_token_balance_before - tokens_to_burn
    );

    // Check validator stake account balance
    let validator_stake_account =
        get_account(&mut banks_client, &validator_stake_account.stake_account).await;
    assert_eq!(
        validator_stake_account.lamports,
        validator_stake_item.stake_lamports
    );

    // Check user recipient stake account balance
    let user_stake_recipient_account =
        get_account(&mut banks_client, &user_stake_recipient.pubkey()).await;
    assert_eq!(
        user_stake_recipient_account.lamports,
        initial_stake_lamports + tokens_to_burn
    );
}

#[tokio::test]
async fn test_stake_pool_withdraw_with_wrong_stake_program() {
    let (
        mut banks_client,
        payer,
        recent_blockhash,
        stake_pool_accounts,
        validator_stake_account,
        deposit_info,
        tokens_to_burn,
    ) = setup().await;

    // Create stake account to withdraw to
    let user_stake_recipient = Keypair::new();

    let new_authority = Pubkey::new_unique();
    let wrong_stake_program = Pubkey::new_unique();

    let accounts = vec![
        AccountMeta::new(stake_pool_accounts.stake_pool.pubkey(), false),
        AccountMeta::new(stake_pool_accounts.validator_list.pubkey(), false),
        AccountMeta::new_readonly(stake_pool_accounts.withdraw_authority, false),
        AccountMeta::new(validator_stake_account.stake_account, false),
        AccountMeta::new(user_stake_recipient.pubkey(), false),
        AccountMeta::new_readonly(new_authority, false),
        AccountMeta::new(deposit_info.user_pool_account, false),
        AccountMeta::new(stake_pool_accounts.pool_mint.pubkey(), false),
        AccountMeta::new_readonly(sysvar::clock::id(), false),
        AccountMeta::new_readonly(spl_token::id(), false),
        AccountMeta::new_readonly(wrong_stake_program, false),
    ];
    let instruction = Instruction {
        program_id: id(),
        accounts,
        data: instruction::StakePoolInstruction::Withdraw(tokens_to_burn)
            .try_to_vec()
            .unwrap(),
    };

    let mut transaction = Transaction::new_with_payer(&[instruction], Some(&payer.pubkey()));
    transaction.sign(&[&payer], recent_blockhash);
    let transaction_error = banks_client
        .process_transaction(transaction)
        .await
        .err()
        .unwrap();

    match transaction_error {
        TransportError::TransactionError(TransactionError::InstructionError(_, error)) => {
            assert_eq!(error, InstructionError::IncorrectProgramId);
        }
        _ => panic!("Wrong error occurs while try to withdraw with wrong stake program ID"),
    }
}

#[tokio::test]
async fn test_stake_pool_withdraw_with_wrong_withdraw_authority() {
    let (
        mut banks_client,
        payer,
        recent_blockhash,
        mut stake_pool_accounts,
        validator_stake_account,
        deposit_info,
        tokens_to_burn,
    ) = setup().await;

    // Create stake account to withdraw to
    let user_stake_recipient = Keypair::new();

    let new_authority = Pubkey::new_unique();
    stake_pool_accounts.withdraw_authority = Keypair::new().pubkey();

    let transaction_error = stake_pool_accounts
        .withdraw_stake(
            &mut banks_client,
            &payer,
            &recent_blockhash,
            &user_stake_recipient.pubkey(),
            &deposit_info.user_pool_account,
            &validator_stake_account.stake_account,
            &new_authority,
            tokens_to_burn,
        )
        .await
        .err()
        .unwrap();

    match transaction_error {
        TransportError::TransactionError(TransactionError::InstructionError(
            _,
            InstructionError::Custom(error_index),
        )) => {
            let program_error = error::StakePoolError::InvalidProgramAddress as u32;
            assert_eq!(error_index, program_error);
        }
        _ => panic!("Wrong error occurs while try to withdraw with wrong withdraw authority"),
    }
}

#[tokio::test]
async fn test_stake_pool_withdraw_with_wrong_token_program_id() {
    let (
        mut banks_client,
        payer,
        recent_blockhash,
        stake_pool_accounts,
        validator_stake_account,
        deposit_info,
        tokens_to_burn,
    ) = setup().await;

    // Create stake account to withdraw to
    let user_stake_recipient = Keypair::new();

    let new_authority = Pubkey::new_unique();
    let wrong_token_program = Keypair::new();

    let mut transaction = Transaction::new_with_payer(
        &[instruction::withdraw(
            &id(),
            &stake_pool_accounts.stake_pool.pubkey(),
            &stake_pool_accounts.validator_list.pubkey(),
            &stake_pool_accounts.withdraw_authority,
            &validator_stake_account.stake_account,
            &user_stake_recipient.pubkey(),
            &new_authority,
            &deposit_info.user_pool_account,
            &stake_pool_accounts.pool_mint.pubkey(),
            &wrong_token_program.pubkey(),
            tokens_to_burn,
        )
        .unwrap()],
        Some(&payer.pubkey()),
    );
    transaction.sign(&[&payer], recent_blockhash);
    let transaction_error = banks_client
        .process_transaction(transaction)
        .await
        .err()
        .unwrap();

    match transaction_error {
        TransportError::TransactionError(TransactionError::InstructionError(_, error)) => {
            assert_eq!(error, InstructionError::IncorrectProgramId);
        }
        _ => panic!("Wrong error occurs while try to withdraw with wrong token program ID"),
    }
}

#[tokio::test]
async fn test_stake_pool_withdraw_with_wrong_validator_list() {
    let (
        mut banks_client,
        payer,
        recent_blockhash,
        mut stake_pool_accounts,
        validator_stake_account,
        deposit_info,
        tokens_to_burn,
    ) = setup().await;

    // Create stake account to withdraw to
    let user_stake_recipient = Keypair::new();

    let new_authority = Pubkey::new_unique();
    stake_pool_accounts.validator_list = Keypair::new();

    let transaction_error = stake_pool_accounts
        .withdraw_stake(
            &mut banks_client,
            &payer,
            &recent_blockhash,
            &user_stake_recipient.pubkey(),
            &deposit_info.user_pool_account,
            &validator_stake_account.stake_account,
            &new_authority,
            tokens_to_burn,
        )
        .await
        .err()
        .unwrap();

    match transaction_error {
        TransportError::TransactionError(TransactionError::InstructionError(
            _,
            InstructionError::Custom(error_index),
        )) => {
            let program_error = error::StakePoolError::InvalidValidatorStakeList as u32;
            assert_eq!(error_index, program_error);
        }
        _ => panic!(
            "Wrong error occurs while try to withdraw with wrong validator stake list account"
        ),
    }
}

#[tokio::test]
async fn test_stake_pool_withdraw_from_unknown_validator() {
    let (mut banks_client, payer, recent_blockhash) = program_test().start().await;
    let stake_pool_accounts = StakePoolAccounts::new();
    stake_pool_accounts
        .initialize_stake_pool(&mut banks_client, &payer, &recent_blockhash)
        .await
        .unwrap();

    let validator_stake_account = ValidatorStakeAccount::new_with_target_authority(
        &stake_pool_accounts.deposit_authority,
        &stake_pool_accounts.stake_pool.pubkey(),
    );
    validator_stake_account
        .create_and_delegate(
            &mut banks_client,
            &payer,
            &recent_blockhash,
            &stake_pool_accounts.staker,
        )
        .await;

    let user_stake = ValidatorStakeAccount::new_with_target_authority(
        &stake_pool_accounts.deposit_authority,
        &stake_pool_accounts.stake_pool.pubkey(),
    );
    user_stake
        .create_and_delegate(
            &mut banks_client,
            &payer,
            &recent_blockhash,
            &stake_pool_accounts.staker,
        )
        .await;

    let user_pool_account = Keypair::new();
    let user = Keypair::new();
    create_token_account(
        &mut banks_client,
        &payer,
        &recent_blockhash,
        &user_pool_account,
        &stake_pool_accounts.pool_mint.pubkey(),
        &user.pubkey(),
    )
    .await
    .unwrap();

    let user = Keypair::new();
    // make stake account
    let user_stake = Keypair::new();
    let lockup = stake_program::Lockup::default();
    let authorized = stake_program::Authorized {
        staker: stake_pool_accounts.deposit_authority,
        withdrawer: stake_pool_accounts.deposit_authority,
    };
    create_independent_stake_account(
        &mut banks_client,
        &payer,
        &recent_blockhash,
        &user_stake,
        &authorized,
        &lockup,
    )
    .await;
    // make pool token account
    let user_pool_account = Keypair::new();
    create_token_account(
        &mut banks_client,
        &payer,
        &recent_blockhash,
        &user_pool_account,
        &stake_pool_accounts.pool_mint.pubkey(),
        &user.pubkey(),
    )
    .await
    .unwrap();

    let user_pool_account = user_pool_account.pubkey();
    let pool_tokens = get_token_balance(&mut banks_client, &user_pool_account).await;

    let tokens_to_burn = pool_tokens / 4;

    // Delegate tokens for burning
    delegate_tokens(
        &mut banks_client,
        &payer,
        &recent_blockhash,
        &user_pool_account,
        &user,
        &stake_pool_accounts.withdraw_authority,
        tokens_to_burn,
    )
    .await;

    // Create stake account to withdraw to
    let user_stake_recipient = Keypair::new();

    let new_authority = Pubkey::new_unique();

    let transaction_error = stake_pool_accounts
        .withdraw_stake(
            &mut banks_client,
            &payer,
            &recent_blockhash,
            &user_stake_recipient.pubkey(),
            &user_pool_account,
            &validator_stake_account.stake_account,
            &new_authority,
            tokens_to_burn,
        )
        .await
        .err()
        .unwrap();

    match transaction_error {
        TransportError::TransactionError(TransactionError::InstructionError(
            _,
            InstructionError::Custom(error_index),
        )) => {
            let program_error = error::StakePoolError::ValidatorNotFound as u32;
            assert_eq!(error_index, program_error);
        }
        _ => panic!("Wrong error occurs while try to do withdraw from unknown validator"),
    }
}

#[tokio::test]
async fn test_stake_pool_double_withdraw_to_the_same_account() {
    let (
        mut banks_client,
        payer,
        recent_blockhash,
        stake_pool_accounts,
        validator_stake_account,
        deposit_info,
        tokens_to_burn,
    ) = setup().await;

    // Create stake account to withdraw to
    let user_stake_recipient = Keypair::new();
    create_blank_stake_account(
        &mut banks_client,
        &payer,
        &recent_blockhash,
        &user_stake_recipient,
    )
    .await;

    let new_authority = Pubkey::new_unique();
    stake_pool_accounts
        .withdraw_stake(
            &mut banks_client,
            &payer,
            &recent_blockhash,
            &user_stake_recipient.pubkey(),
            &deposit_info.user_pool_account,
            &validator_stake_account.stake_account,
            &new_authority,
            tokens_to_burn,
        )
        .await
        .unwrap();

    let latest_blockhash = banks_client.get_recent_blockhash().await.unwrap();

    let transaction_error = stake_pool_accounts
        .withdraw_stake(
            &mut banks_client,
            &payer,
            &latest_blockhash,
            &user_stake_recipient.pubkey(),
            &deposit_info.user_pool_account,
            &validator_stake_account.stake_account,
            &new_authority,
            tokens_to_burn,
        )
        .await
        .err()
        .unwrap();

    match transaction_error {
        TransportError::TransactionError(TransactionError::InstructionError(_, error)) => {
            assert_eq!(error, InstructionError::InvalidAccountData);
        }
        _ => panic!("Wrong error occurs while try to do double withdraw"),
    }
}

#[tokio::test]
async fn test_stake_pool_withdraw_token_delegate_was_not_setup() {
    let (mut banks_client, payer, recent_blockhash) = program_test().start().await;
    let stake_pool_accounts = StakePoolAccounts::new();
    stake_pool_accounts
        .initialize_stake_pool(&mut banks_client, &payer, &recent_blockhash)
        .await
        .unwrap();

    let validator_stake_account: ValidatorStakeAccount = simple_add_validator_to_pool(
        &mut banks_client,
        &payer,
        &recent_blockhash,
        &stake_pool_accounts,
    )
    .await;

    let deposit_info: DepositInfo = simple_deposit(
        &mut banks_client,
        &payer,
        &recent_blockhash,
        &stake_pool_accounts,
        &validator_stake_account,
    )
    .await;

    let tokens_to_burn = deposit_info.pool_tokens / 4;

    // Create stake account to withdraw to
    let user_stake_recipient = Keypair::new();
    create_blank_stake_account(
        &mut banks_client,
        &payer,
        &recent_blockhash,
        &user_stake_recipient,
    )
    .await;

    let new_authority = Pubkey::new_unique();
    let transaction_error = stake_pool_accounts
        .withdraw_stake(
            &mut banks_client,
            &payer,
            &recent_blockhash,
            &user_stake_recipient.pubkey(),
            &deposit_info.user_pool_account,
            &validator_stake_account.stake_account,
            &new_authority,
            tokens_to_burn,
        )
        .await
        .err()
        .unwrap();

    match transaction_error {
        TransportError::TransactionError(TransactionError::InstructionError(
            _,
            InstructionError::Custom(error_index),
        )) => {
            let program_error = TokenError::OwnerMismatch as u32;
            assert_eq!(error_index, program_error);
        }
        _ => panic!(
            "Wrong error occurs while try to do withdraw without token delegation for burn before"
        ),
    }
}

#[tokio::test]
async fn test_stake_pool_withdraw_with_low_delegation() {
    let (mut banks_client, payer, recent_blockhash) = program_test().start().await;
    let stake_pool_accounts = StakePoolAccounts::new();
    stake_pool_accounts
        .initialize_stake_pool(&mut banks_client, &payer, &recent_blockhash)
        .await
        .unwrap();

    let validator_stake_account: ValidatorStakeAccount = simple_add_validator_to_pool(
        &mut banks_client,
        &payer,
        &recent_blockhash,
        &stake_pool_accounts,
    )
    .await;

    let deposit_info: DepositInfo = simple_deposit(
        &mut banks_client,
        &payer,
        &recent_blockhash,
        &stake_pool_accounts,
        &validator_stake_account,
    )
    .await;

    let tokens_to_burn = deposit_info.pool_tokens / 4;

    // Delegate tokens for burning
    delegate_tokens(
        &mut banks_client,
        &payer,
        &recent_blockhash,
        &deposit_info.user_pool_account,
        &deposit_info.user,
        &stake_pool_accounts.withdraw_authority,
        1,
    )
    .await;

    // Create stake account to withdraw to
    let user_stake_recipient = Keypair::new();
    create_blank_stake_account(
        &mut banks_client,
        &payer,
        &recent_blockhash,
        &user_stake_recipient,
    )
    .await;

    let new_authority = Pubkey::new_unique();
    let transaction_error = stake_pool_accounts
        .withdraw_stake(
            &mut banks_client,
            &payer,
            &recent_blockhash,
            &user_stake_recipient.pubkey(),
            &deposit_info.user_pool_account,
            &validator_stake_account.stake_account,
            &new_authority,
            tokens_to_burn,
        )
        .await
        .err()
        .unwrap();

    match transaction_error {
        TransportError::TransactionError(TransactionError::InstructionError(
            _,
            InstructionError::Custom(error_index),
        )) => {
            let program_error = TokenError::InsufficientFunds as u32;
            assert_eq!(error_index, program_error);
        }
        _ => panic!(
            "Wrong error occurs while try to do withdraw with not enough delegated tokens to burn"
        ),
    }
}
