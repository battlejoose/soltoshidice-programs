//! Atomic base settlement of frozen ER exits.
use crate::*;

pub fn settle<'info>(ctx: Context<'info, Close<'info>>, sell_count: u8, min_swap_out: u64, market_accounts:u8, fee_sell_count:u8, perp_buy_count:u8, min_perp:u64, min_gas:u64, sol_quote: SolUsdQuote) -> Result<()> {
        require!(ctx.accounts.pool_accounts.pool.id == 0, MixError::Binding);
        let same = ctx.accounts.asset.key() == ctx.accounts.pool_accounts.pool.settlement;
        require!(if same {ctx.accounts.settlement_inventory.is_none() && ctx.accounts.settlement_vault.is_none()} else {ctx.accounts.settlement_inventory.is_some() && ctx.accounts.settlement_vault.is_some()},MixError::Binding);
        let treasury_before = if same {ctx.accounts.inventory.treasury} else {ctx.accounts.settlement_inventory.as_ref().ok_or(MixError::Binding)?.treasury};
        let tracked_before = pump::sell_value(treasury_before,&ctx.accounts.settlement,&ctx.accounts.ownership.settlement_valuation)? as i128 - ctx.accounts.pool_accounts.pool.queue_usd as i128;
        ctx.accounts.ownership.validate(&ctx.accounts.position, ctx.accounts.pool_accounts.pool.key())?;
        let trader = basebridge::committed_trader(&ctx.accounts.bridge.trader, ctx.accounts.position.owner,
            ctx.accounts.bridge.config.key(), ctx.accounts.pool_accounts.pool.key())?;
        let intent = basebridge::pending_intent(&trader, &ctx.accounts.bridge.custody, ctx.accounts.position.er_slot,
            ctx.accounts.position.nonce, 1, ctx.accounts.bridge.config.next_settlement_seq)?;
        basebridge::validate_staged(&ctx.accounts.position, &intent, &ctx.accounts.market, ctx.accounts.asset.key())?;
        let x = &mut ctx.accounts.position;
        require!(x.state == 0, MixError::Closed);
        let liquidated = intent.liquidated;
        let (win, token_pnl) = math::pnl(x.margin, x.entry, intent.exit, x.leverage, x.long)?;
        require!((intent.close_seq > 0) == (win && !liquidated), MixError::QueueOrder);
        let loss = if liquidated {
            x.margin
        } else if win {
            0
        } else {
            token_pnl
        };
        let returned = x.margin - loss;
        let (mut acid_input,mut gas_share)=math::loss_split(loss)?;
        // Tiny unswappable allocations stay in the pool, with no reward or burn.
        let small_loss=loss>0 && pump::sell_value(loss,&ctx.accounts.asset,&ctx.accounts.pool_accounts.valuation)?<1000;
        if small_loss {acid_input=loss;gas_share=0;}
        let coverage = {
            sol_quote.validate(Clock::get()?.unix_timestamp)?;
            let current = pump::collateral_value(x.margin, &ctx.accounts.asset, &ctx.accounts.pool_accounts.valuation, sol_quote.price)?;
            let target = if crate::stake_backing::requires_backing(ctx.accounts.asset.key()) {
                crate::stake_backing::loss_shortfall(x.entry_value, current, loss, x.margin)?
            } else { 0 };
            let curve = pump::collateral_curve(&ctx.accounts.settlement, &ctx.accounts.ownership.settlement_valuation, sol_quote.price)?;
            let available = crate::stake_backing::available_stake(&ctx.accounts.bridge.custody, &trader)?;
            let coverage = crate::stake_backing::coverage_tokens(available, target, &mut |n| curve.value(n))?;
            x.collateral_close_value = current;
            x.coverage_tokens = coverage.tokens;
            x.coverage_usd = target - coverage.uncovered_usd;
            // Informational pool shortfall, never personal debt or an unlock lock.
            x.uncovered_usd = coverage.uncovered_usd;
            // Winners keep their stake; only the bet tokens set the profit basis.
            x.close_value = x.entry_value.min(current);
            coverage
        };
        let profit = if win && !liquidated { math::pnl(x.close_value, x.entry, intent.exit, x.leverage, x.long)?.1 } else { 0 };
        let inv = &mut ctx.accounts.inventory;
        inv.locked = inv
            .locked
            .checked_sub(loss)
            .ok_or(MixError::Arithmetic)?;
        inv.losses = inv.losses.checked_add(loss).ok_or(MixError::Arithmetic)?;
        if !same {
            require_keys_eq!(ctx.accounts.settlement_inventory.as_ref().unwrap().key(),Pubkey::find_program_address(&[b"inventory",ctx.accounts.pool_accounts.pool.key().as_ref(),ctx.accounts.settlement.key().as_ref()],&crate::ID).0,MixError::Binding);
            require_keys_eq!(ctx.accounts.settlement_vault.as_ref().unwrap().key(),Pubkey::find_program_address(&[b"vault",ctx.accounts.pool_accounts.pool.key().as_ref(),ctx.accounts.settlement.key().as_ref()],&crate::ID).0,MixError::Binding);
        }
        // One collateral sale, one ACIDPUMP buy and (unless already SOL) one SOL buy.
        // Preserve the instruction layout, but reject the obsolete separate 90% route.
        require!(sell_count==0 && market_accounts==0 && min_swap_out>0,MixError::Binding);
        let fees=&mut ctx.accounts.fees;
        require!(ctx.accounts.pool_accounts.pool.id==0 && fees.perp.id==102 && fees.gas_asset.mint==anchor_spl::token::spl_token::native_mint::ID, MixError::Binding);
        require!(fees.main_pool.is_none() && fees.main_inventory.is_none() && fees.main_vault.is_none() && fees.main_ownership_pool.is_none(),MixError::Binding);
        require_keys_eq!(ctx.accounts.settlement.key(),fees.perp.key(),MixError::Binding);
        require_keys_eq!(fees.perp_vault.key(),Pubkey::find_program_address(&[b"vault",ctx.accounts.pool_accounts.pool.key().as_ref(),fees.perp.key().as_ref()],&crate::ID).0,MixError::Binding);
        require_keys_eq!(fees.gas_vault.key(),Pubkey::find_program_address(&[b"gas-vault",ctx.accounts.pool_accounts.pool.key().as_ref()],&crate::ID).0,MixError::Binding);
        let (acid_out,gas_out)=if loss==0 {(0,0)} else if same {
            // ACIDPUMP collateral stays ACIDPUMP. Only its 2% gas share is sold.
            require!(perp_buy_count==0,MixError::Binding);
            let gas=if gas_share>0 {
                pump::swap_loss(&ctx.accounts.pool_accounts.pool,&ctx.accounts.pool_accounts.authority,&ctx.accounts.asset,&fees.gas_asset,
                    &ctx.accounts.vault,&fees.gas_vault,&ctx.accounts.quote_vault,&ctx.accounts.pump_program,
                    ctx.remaining_accounts,fee_sell_count,gas_share,min_gas.max(1))?
            }else{0};
            (acid_input,gas)
        }else if (pump::is_quote(&ctx.accounts.asset) && loss<100) || pump::sell_value(loss,&ctx.accounts.asset,&ctx.accounts.pool_accounts.valuation)?==0 {
            inv.dust=inv.dust.checked_add(loss).ok_or(MixError::Arithmetic)?;
            (0,0)
        }else if ctx.accounts.asset.mint==anchor_spl::token::spl_token::native_mint::ID {
            // Retain native SOL for gas, and convert the other 98% only once.
            require!(perp_buy_count>0 && fee_sell_count as usize+perp_buy_count as usize==ctx.remaining_accounts.len(),MixError::Binding);
            let bought=pump::swap_loss(&ctx.accounts.pool_accounts.pool,&ctx.accounts.pool_accounts.authority,&ctx.accounts.asset,&fees.perp,
                &ctx.accounts.vault,&fees.perp_vault,&ctx.accounts.quote_vault,&ctx.accounts.pump_program,
                ctx.remaining_accounts,fee_sell_count,acid_input,min_perp.max(1))?;
            if gas_share>0 {pump::move_tokens(&ctx.accounts.pool_accounts.pool,&ctx.accounts.pool_accounts.authority,&ctx.accounts.vault,&fees.gas_vault,&fees.gas_program.to_account_info(),gas_share)?;}
            (bought,gas_share)
        }else {
            pump::swap_fees(&ctx.accounts.pool_accounts.pool,&ctx.accounts.pool_accounts.authority,&ctx.accounts.asset,&fees.perp,&fees.gas_asset,
                &ctx.accounts.vault,&fees.perp_vault,&fees.gas_vault,&ctx.accounts.quote_vault,&ctx.accounts.pump_program,
                ctx.remaining_accounts,fee_sell_count,perp_buy_count,loss,gas_share,min_perp,min_gas)?
        };
        let (payout_receipts,staker_out,burn_out)=if small_loss {(acid_out,0,0)} else {math::acid_split(acid_out)?};
        if same {inv.treasury=inv.treasury.checked_add(payout_receipts).ok_or(MixError::Arithmetic)?;}
        else {
            let target=ctx.accounts.settlement_inventory.as_mut().ok_or(MixError::Binding)?;
            require!(target.pool==ctx.accounts.pool_accounts.pool.key() && target.asset==ctx.accounts.settlement.key(),MixError::Binding);
            target.treasury=target.treasury.checked_add(payout_receipts).ok_or(MixError::Arithmetic)?;
        }
        if staker_out>0 {
            ownership::fund_rewards(&mut ctx.accounts.ownership.ownership_pool,staker_out)?;
            fees.gas_reserve.perp_bought=fees.gas_reserve.perp_bought.checked_add(staker_out).ok_or(MixError::Arithmetic)?;
        }
        if burn_out>0 {
            let pool_key=ctx.accounts.pool_accounts.pool.key();
            let seeds:&[&[u8]]=&[b"authority",pool_key.as_ref(),&[ctx.accounts.pool_accounts.pool.authority_bump]];
            token_interface::burn(CpiContext::new_with_signer(fees.perp_program.key(),token_interface::Burn{
                mint:fees.perp_mint.to_account_info(),from:fees.perp_vault.to_account_info(),authority:ctx.accounts.pool_accounts.authority.to_account_info(),
            },&[seeds]),burn_out)?;
        }
        if gas_out>0 {
            unwrap_gas(&ctx.accounts.pool_accounts.pool,&ctx.accounts.pool_accounts.authority,&fees.gas_vault,&fees.gas_mint,&fees.gas_reserve,&fees.gas_program,&fees.system_program)?;
            fees.gas_reserve.sol_received=fees.gas_reserve.sol_received.checked_add(gas_out).ok_or(MixError::Arithmetic)?;
        }
        // Keep the old event decodable for historical fee-reserve receipts.
        emit!(LossSplit{position:x.key(),loss,market_tokens:acid_input,gas_tokens:gas_share,perp_tokens:0,perp_bought:acid_out,sol_received:gas_out});
        emit!(RevenueSplit{position:x.key(),acid_received:acid_out,pool_tokens:payout_receipts,staker_tokens:staker_out,burned_tokens:burn_out});
        // Both the loss swap and the trader's consumed backing fund the payout
        // pool. Award their combined contribution once, under the same curve.
        let settlement_inventory = if same { &mut ctx.accounts.inventory } else { ctx.accounts.settlement_inventory.as_mut().ok_or(MixError::Binding)? };
        crate::stake_backing::allocate_coverage(&mut ctx.accounts.bridge.custody, &trader, settlement_inventory, coverage)?;
        emit!(CoreCoverageApplied { position: x.key(), owner: x.owner, closing_usd: x.collateral_close_value,
            core_tokens: coverage.tokens, covered_usd: x.coverage_usd, uncovered_usd: coverage.uncovered_usd });
        let ownership_tokens = payout_receipts.checked_add(coverage.tokens).ok_or(MixError::Arithmetic)?;
        let tracked_after = pump::sell_value(settlement_inventory.treasury,&ctx.accounts.settlement,&ctx.accounts.ownership.settlement_valuation)? as i128 - ctx.accounts.pool_accounts.pool.queue_usd as i128;
        let basis_usd = pump::sell_value(ownership_tokens,&ctx.accounts.settlement,&ctx.accounts.ownership.settlement_valuation)?;
        let advance = (tracked_after-tracked_before).max(0).min(basis_usd as i128) as u64;
        ownership::award_scaled(&mut ctx.accounts.ownership.ownership_pool, &mut ctx.accounts.ownership.ownership_trader, &mut ctx.accounts.ownership.ownership_receipt, ownership_tokens,basis_usd,tracked_before,advance)?;
        ownership::sweep_surplus(&mut ctx.accounts.ownership.ownership_pool,settlement_inventory,ctx.accounts.pool_accounts.pool.queue_usd,&ctx.accounts.settlement,&ctx.accounts.ownership.settlement_valuation)?;
        x.swapped_out = payout_receipts;
        x.state = if liquidated { 2 } else { 1 };
        x.closed_at = intent.closed_at;
        x.exit = intent.exit;
        x.loss = loss;
        x.returned = returned;
        x.profit = profit;
        x.remaining = profit;
        let pool = &mut ctx.accounts.pool_accounts.pool;
        if profit > 0 {
            x.queue_seq = pool.queue_tail;
            pool.queue_tail = pool.queue_tail.checked_add(1).ok_or(MixError::Arithmetic)?;
            pool.queue_usd = pool
                .queue_usd
                .checked_add(profit)
                .ok_or(MixError::Arithmetic)?;
        }
        basebridge::finish_trade(&mut ctx.accounts.bridge.custody, x.er_slot, &intent, x.key(), loss)?;
        if intent.close_seq > 0 {
            ctx.accounts.bridge.config.next_settlement_seq = ctx.accounts.bridge.config.next_settlement_seq.checked_add(1).ok_or(MixError::Arithmetic)?;
        }
        pool.trades = pool.trades.checked_add(1).ok_or(MixError::Arithmetic)?;
        emit!(Settled {
            position: x.key(),
            profit_usd: profit,
            loss,
            returned,
            liquidated
        });
        Ok(())
    }
