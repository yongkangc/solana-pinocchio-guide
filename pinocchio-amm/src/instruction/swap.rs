use bytemuck::{Pod, Zeroable};
use constant_product_curve::{ConstantProduct, LiquidityPair};
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
pub struct SwapInstructionData {
    pub is_x: u8,
    pub amount: [u8; 8],
    pub min: [u8; 8],
}

impl SwapInstructionData {
    pub const LEN: usize = core::mem::size_of::<SwapInstructionData>();
}

pub fn process_swap(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [user, mint_x, mint_y, vault_x, vault_y, user_x, user_y, mint_lp, user_lp, config, _system_program, _token_program] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !user.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }

    let instruction_data = bytemuck::try_from_bytes::<SwapInstructionData>(&data)
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
    let is_x = match instruction_data.is_x {
        0 => false,
        1 => true,
        _ => return Err(ProgramError::InvalidAccountData),
    };

    let mut curve = ConstantProduct::init(
        vault_x_amount,
        vault_y_amount,
        mint_lp_supply,
        u16::from_le_bytes(config_account.fee),
        None,
    )
    .map_err(|_| CustomError::InvalidSwap)?;

    let p = match is_x {
        true => LiquidityPair::X,
        false => LiquidityPair::Y,
    };

    let res = curve
        .swap(p, amount, u64::from_le_bytes(instruction_data.min))
        .map_err(|_| CustomError::InvalidSwap)?;

    // Setup signer seeds.
    let bump = [config_account.config_bump];
    let seed = [
        Seed::from(CONFIG_SEED.as_bytes()),
        Seed::from(config_account.seed.as_ref()),
        Seed::from(&bump),
    ];
    let seeds = Signer::from(&seed);

    // Deposit to pool.
    if is_x {
        pinocchio_token::instructions::TransferChecked {
            from: user_x,
            mint: mint_x,
            to: vault_x,
            authority: user,
            amount: res.deposit,
            decimals: mint_x_account.decimals(),
        }
        .invoke()?;
    } else {
        pinocchio_token::instructions::TransferChecked {
            from: user_y,
            mint: mint_y,
            to: vault_y,
            authority: user,
            amount: res.deposit,
            decimals: mint_y_account.decimals(),
        }
        .invoke()?;
    }

    // Withdraw from pool.
    if is_x {
        pinocchio_token::instructions::TransferChecked {
            from: vault_y,
            mint: mint_y,
            to: user_y,
            authority: config,
            amount: res.withdraw,
            decimals: mint_y_account.decimals(),
        }
        .invoke_signed(&[seeds.clone()])?;
    } else {
        pinocchio_token::instructions::TransferChecked {
            from: vault_x,
            mint: mint_x,
            to: user_x,
            authority: config,
            amount: res.withdraw,
            decimals: mint_x_account.decimals(),
        }
        .invoke_signed(&[seeds.clone()])?;
    }

    Ok(())
}
