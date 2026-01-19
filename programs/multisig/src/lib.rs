use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::Instruction;
use anchor_lang::solana_program::program::invoke_signed;
use std::collections::BTreeSet;

declare_id!("38tdFSkJASspVp8GvqdwjLiHTK2crbubsC75d1q31EPo");

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct SerializableAccountMeta {
    pub pubkey: Pubkey,
    pub is_signer: bool,
    pub is_writable: bool,
}

impl From<AccountMeta> for SerializableAccountMeta {
    fn from(meta: AccountMeta) -> Self {
        Self {
            pubkey: meta.pubkey,
            is_signer: meta.is_signer,
            is_writable: meta.is_writable,
        }
    }
}

impl From<SerializableAccountMeta> for AccountMeta {
    fn from(s: SerializableAccountMeta) -> Self {
        Self {
            pubkey: s.pubkey,
            is_signer: s.is_signer,
            is_writable: s.is_writable,
        }
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InstructionData {
    pub program_id: Pubkey,
    pub accounts: Vec<SerializableAccountMeta>,
    pub data: Vec<u8>,
}

#[account]
pub struct Multisig {
    pub creator: Pubkey,      
    pub nonce: u8,  
    pub members: Vec<Pubkey>,
    pub threshold: u8,
    pub proposals_count: u32, // 用于生成唯一 Proposal PDA
    pub bump: u8,
}

#[account]
pub struct Proposal {
    pub multisig: Pubkey,
    pub proposer: Pubkey,
    pub instruction: InstructionData,
    pub approvals: Vec<Pubkey>,
    pub executed: bool,
    pub cancelled: bool,
    pub bump: u8,
}

#[error_code]
pub enum MultisigError {
    #[msg("Members must be sorted and unique")]
    InvalidMembers,
    #[msg("Threshold out of range")]
    InvalidThreshold,
    #[msg("Only members can interact")]
    NotMember,
    #[msg("Already approved")]
    AlreadyApproved,
    #[msg("Not enough approvals")]
    NotExecutable,
    #[msg("Only proposer can cancel")]
    NotProposer,
    #[msg("Already processed")]
    AlreadyProcessed,
    #[msg("CPI account mismatch")]
    AccountMismatch,
}

// ===== Accounts =====

#[derive(Accounts)]
#[instruction(nonce: u8, members: Vec<Pubkey>, threshold: u8)]
pub struct CreateMultisig<'info> {
    #[account(
        init,
        seeds = [b"multisig", creator.key().as_ref(), &[nonce]],
        bump,
        payer = creator,
        space = 8 + 32 + 1 + (32 * 10) + 1 + 4 + 1
        //       ^   ^    ^     ^        ^    ^    ^
        //       |   |    |     |        |    |    |
        //       |   |    |     |        |    |    bump
        //       |   |    |     |        |    proposals_count (u32)
        //       |   |    |     |        threshold (u8)
        //       |   |    |     members (max 10)
        //       |   |    nonce (u8)
        //       |   creator (Pubkey = 32)
        //       discriminator (8)
    )]
    pub multisig: Account<'info, Multisig>,
    #[account(mut)]
    pub creator: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(instruction_data: InstructionData)]
pub struct ProposeTransaction<'info> {
    #[account(mut)]
    pub multisig: Account<'info, Multisig>,
    #[account(
        init,
        seeds = [b"proposal", multisig.key().as_ref(), &multisig.proposals_count.to_le_bytes()],
        bump,
        payer = proposer,
        space = 8 + 32 + 32 + 1000 + (32 * 10) + 1 + 1 + 1
    )]
    pub proposal: Account<'info, Proposal>,
    #[account(mut)]
    pub proposer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ApproveTransaction<'info> {
    #[account(mut)]
    pub multisig: Account<'info, Multisig>,
    #[account(
        mut,
        constraint = proposal.multisig == multisig.key(),
        constraint = !proposal.executed && !proposal.cancelled @ MultisigError::AlreadyProcessed,
    )]
    pub proposal: Account<'info, Proposal>,
    #[account(
        constraint = multisig.members.contains(&approver.key()) @ MultisigError::NotMember,
    )]
    pub approver: Signer<'info>,
}

#[derive(Accounts)]
pub struct ExecuteTransaction<'info> {
    #[account(mut)]
    pub multisig: Account<'info, Multisig>,
    #[account(
        mut,
        close = multisig,
        constraint = proposal.multisig == multisig.key(),
        constraint = !proposal.executed && !proposal.cancelled @ MultisigError::AlreadyProcessed,
        constraint = {
            let approval_set: BTreeSet<_> = proposal.approvals.iter().collect();
            approval_set.len() >= multisig.threshold as usize
        } @ MultisigError::NotExecutable,
    )]
    pub proposal: Account<'info, Proposal>,
}

#[derive(Accounts)]
pub struct CancelTransaction<'info> {
    #[account(mut)]
    pub multisig: Account<'info, Multisig>,
    #[account(
        mut,
        close = multisig,
        constraint = proposal.multisig == multisig.key(),
        constraint = !proposal.executed && !proposal.cancelled @ MultisigError::AlreadyProcessed,
        constraint = proposal.proposer == canceller.key() @ MultisigError::NotProposer,
    )]
    pub proposal: Account<'info, Proposal>,
    pub canceller: Signer<'info>,
}

// ===== Program Logic =====

#[program]
pub mod multisig {
    use super::*;

    pub fn create_multisig(
        ctx: Context<CreateMultisig>,
        nonce: u8, // used in seeds, not in logic
        members: Vec<Pubkey>,
        threshold: u8,
    ) -> Result<()> {
        // 验证成员：排序 + 唯一 + 非空
        let mut members = members;
        members.sort();
        members.dedup();
        require!(!members.is_empty(), MultisigError::InvalidMembers);
        require!(threshold > 0 && threshold <= members.len() as u8, MultisigError::InvalidThreshold);

        let multisig = &mut ctx.accounts.multisig;
        multisig.creator = ctx.accounts.creator.key(); 
        multisig.nonce = nonce;  
        multisig.members = members;
        multisig.threshold = threshold;
        multisig.proposals_count = 0;
        multisig.bump = ctx.bumps.multisig;
        Ok(())
    }

    pub fn propose_transaction(
        ctx: Context<ProposeTransaction>,
        instruction_data: InstructionData,
    ) -> Result<()> {
        let proposer = ctx.accounts.proposer.key();
        let multisig = &ctx.accounts.multisig;
        require!(multisig.members.contains(&proposer), MultisigError::NotMember);

        let proposal = &mut ctx.accounts.proposal;
        proposal.multisig = multisig.key();
        proposal.proposer = proposer;
        proposal.instruction = instruction_data;
        proposal.approvals = vec![];
        proposal.executed = false;
        proposal.cancelled = false;
        proposal.bump = ctx.bumps.proposal;

        // 递增计数器（防重放）
        ctx.accounts.multisig.proposals_count += 1;
        Ok(())
    }

    pub fn approve_transaction(ctx: Context<ApproveTransaction>) -> Result<()> {
        let approver = ctx.accounts.approver.key();
        let proposal = &mut ctx.accounts.proposal;

        if proposal.approvals.contains(&approver) {
            return err!(MultisigError::AlreadyApproved);
        }

        proposal.approvals.push(approver);
        Ok(())
    }

    pub fn execute_transaction(ctx: Context<ExecuteTransaction>) -> Result<()> {
        let proposal = &mut ctx.accounts.proposal;
        proposal.executed = true;

        let ix = &proposal.instruction;
        let account_infos = ctx.remaining_accounts;
        let accounts: Vec<AccountMeta> = ix.accounts.iter().map(|s| s.clone().into()).collect();

        msg!("Accounts len: {}, AccountInfos len: {}", accounts.len(), account_infos.len());

        // 安全验证 remaining_accounts
        require!(accounts.len() == account_infos.len(), MultisigError::AccountMismatch);
        for (meta, info) in accounts.iter().zip(account_infos.iter()) {
            msg!("meta key: {}, info key: {}", meta.pubkey, *info.key);
            msg!("meta writable: {}, info writable: {}", meta.is_writable, info.is_writable);

            require!(meta.pubkey == *info.key, MultisigError::AccountMismatch);
            //require!(meta.is_writable == info.is_writable, MultisigError::AccountMismatch);
            //require!(meta.is_signer == info.is_signer, MultisigError::AccountMismatch);
        }

        let instruction = Instruction {
            program_id: ix.program_id,
            accounts,
            data: ix.data.clone(),
        };

        // 构造 seeds 并调用 invoke_signed
        let seeds = &[
            b"multisig",
            ctx.accounts.multisig.creator.as_ref(),
            &[ctx.accounts.multisig.nonce],
            &[ctx.accounts.multisig.bump],
        ];
        let signer_seeds = &[&seeds[..]];

        invoke_signed(&instruction, account_infos, signer_seeds)?;
        
        Ok(())
    }

    pub fn cancel_transaction(_ctx: Context<CancelTransaction>) -> Result<()> {
        // 提案账户已在 #[account(close = multisig)] 中自动关闭
        Ok(())
    }
}