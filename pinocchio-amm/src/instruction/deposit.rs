use bytemuck::{Pod, Zeroable};
use constant_product_curve::ConstantProduct;
use pinocchio::{
    account_info::AccountInfo,
    instruction::{Seed, Signer},
    program_error::ProgramError,
    pubkey, ProgramResult,
};
use pinocchio_token::state::{Mint, TokenAccount};

use crate::{constants::CONFIG_SEED, error::CustomError, state::Config};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct DepositInstructionData {
    pub amount: [u8; 8],
    pub max_x: [u8; 8],
    pub max_y: [u8; 8],
}

impl DepositInstructionData {
    pub const LEN: usize = core::mem::size_of::<DepositInstructionData>();
}

pub fn process_deposit(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [user, mint_x, mint_y, vault_x, vault_y, user_x, user_y, mint_lp, user_lp, config, _system_program, _token_program] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !user.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }

    let instruction_data = bytemuck::try_from_bytes::<DepositInstructionData>(&data)
        .map_err(|_| ProgramError::InvalidInstructionData)?;

    let config_account = Config::load(config)?;

    // Validate config account.
    let config_pda = pubkey::create_program_address(
        &[
            CONFIG_SEED.as_bytes(),
            config_account.seed.as_ref(),
            &[config_account.config_bump],
        ],
        &crate::ID,
    )?;
    if config.key() != &config_pda {
        return Err(ProgramError::InvalidAccountData);
    }

    // Validate vault accounts.
    let vault_x_amount;
    let vault_y_amount;
    {
        let vault_x_account = TokenAccount::from_account_info(vault_x)?;
        let vault_y_account = TokenAccount::from_account_info(vault_y)?;
        if vault_x_account.owner() != config.key() {
            return Err(ProgramError::InvalidAccountData);
        }
        if vault_y_account.owner() != config.key() {
            return Err(ProgramError::InvalidAccountData);
        }
        vault_x_amount = vault_x_account.amount();
        vault_y_amount = vault_y_account.amount();
    }

    // Validate mint_lp account.
    let mint_lp_supply;
    {
        let mint_lp_account = Mint::from_account_info(mint_lp)?;
        if mint_lp_account.mint_authority() != Some(config.key()) {
            return Err(ProgramError::InvalidAccountData);
        }
        mint_lp_supply = mint_lp_account.supply();
    }

    // Validate user accounts.
    {
        let user_x_account = TokenAccount::from_account_info(user_x)?;
        let user_y_account = TokenAccount::from_account_info(user_y)?;
        let user_lp_account = TokenAccount::from_account_info(user_lp)?;
        if user_x_account.owner() != user.key() {
            return Err(ProgramError::InvalidAccountData);
        }
        if user_y_account.owner() != user.key() {
            return Err(ProgramError::InvalidAccountData);
        }
        if user_lp_account.owner() != user.key() {
            return Err(ProgramError::InvalidAccountData);
        }
    }

    let mint_x_account = Mint::from_account_info(mint_x)?;
    let mint_y_account = Mint::from_account_info(mint_y)?;

    let amount = u64::from_le_bytes(instruction_data.amount);
    let max_x = u64::from_le_bytes(instruction_data.max_x);
    let max_y = u64::from_le_bytes(instruction_data.max_y);

    let (x, y) = match mint_lp_supply == 0 && vault_x_amount == 0 && vault_y_amount == 0 {
        true => (max_x, max_y),
        false => {
            let amounts = ConstantProduct::xy_deposit_amounts_from_l(
                vault_x_amount,
                vault_y_amount,
                mint_lp_supply,
                amount,
                6,
            )
            .map_err(|_| CustomError::InvalidDeposit)?;
            (amounts.x, amounts.y)
        }
    };

    // Deposit mint_x tokens.
    pinocchio_token::instructions::TransferChecked {
        from: user_x,
        mint: mint_x,
        to: vault_x,
        authority: user,
        amount: x,
        decimals: mint_x_account.decimals(),
    }
    .invoke()?;

    // Deposit mint_y tokens.
    pinocchio_token::instructions::TransferChecked {
        from: user_y,
        mint: mint_y,
        to: vault_y,
        authority: user,
        amount: y,
        decimals: mint_y_account.decimals(),
    }
    .invoke()?;

    // Mint mint_lp tokens.
    let bump = [config_account.config_bump];
    let seed = [
        Seed::from(CONFIG_SEED.as_bytes()),
        Seed::from(config_account.seed.as_ref()),
        Seed::from(&bump),
    ];
    let seeds = Signer::from(&seed);
    pinocchio_token::instructions::MintTo {
        mint: mint_lp,
        account: user_lp,
        mint_authority: config,
        amount: amount,
    }
    .invoke_signed(&[seeds.clone()])?;

    Ok(())
}
