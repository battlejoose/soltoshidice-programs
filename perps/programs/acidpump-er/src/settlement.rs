//! Atomic base settlement of frozen ER exits.
use crate::*;

pub fn settle<'info>(ctx: Context<'info, Close<'info>>, sell_count: u8, min_swap_out: u64, market_accounts:u8, fee_sell_count:u8, perp_buy_count:u8, min_perp:u64, min_gas:u64, sol_quote: SolUsdQuote, backing_routes:Option<BackingRoutes>) -> Result<()> {
        let routes=backing_routes.unwrap_or_default();
        let coverage_count=usize::from(routes.sol.accounts)+usize::from(routes.usd.accounts);
        require!(coverage_count<=ctx.remaining_accounts.len(),MixError::Binding);
        let (coverage_accounts,accounts)=ctx.remaining_accounts.split_at(coverage_count);
        require!(ctx.accounts.pool_accounts.pool.id == 0, MixError::Binding);
        let same = ctx.accounts.asset.key() == ctx.accounts.pool_accounts.pool.settlement;
        require!(if same {ctx.accounts.settlement_inventory.is_none() && ctx.accounts.settlement_vault.is_none()} else {ctx.accounts.settlement_inventory.is_some() && ctx.accounts.settlement_vault.is_some()},MixError::Binding);
        let treasury_before = if same {ctx.accounts.inventory.treasury} else {ctx.accounts.settlement_inventory.as_ref().ok_or(MixError::Binding)?.treasury};
        let pool_usd_before = pump::spot_value(treasury_before,&ctx.accounts.settlement,&ctx.accounts.ownership.settlement_valuation)?;
        let tracked_before = pool_usd_before as i128;
        ctx.accounts.ownership.validate(&ctx.accounts.position, ctx.accounts.pool_accounts.pool.key())?;
        let trader = basebridge::committed_trader(&ctx.accounts.bridge.trader, ctx.accounts.position.owner,
            ctx.accounts.bridge.config.key(), ctx.accounts.pool_accounts.pool.key())?;
        // Until the mixed settlement entrypoint is used, never silently skip
        // auxiliary backing or release a receipt with an incomplete charge.
        require!(backing_routes.is_some() || trader.auxiliary_stakes.iter().all(|s|s.tokens==0),MixError::Binding);
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
        // Quote before swaps change reserves; never derive rewards from the
        // pool/staker/burn split or FIFO state. All trader loss counts.
        let loss_reward_tokens=pump::loss_reward_tokens(loss,&ctx.accounts.asset,&ctx.accounts.pool_accounts.valuation,
            &ctx.accounts.settlement,&ctx.accounts.ownership.settlement_valuation)?;
        // A queued venue retains all post-gas proceeds: neither burn is funded.
        let root = crate::venue::is_root(&ctx.accounts.bridge.config.key());
        #[cfg(feature="mainnet")]
        let native=crate::native_market::kind(&ctx.accounts.settlement)?.is_some();
        #[cfg(not(feature="mainnet"))]
        let native=false;
        let pool_before=&ctx.accounts.pool_accounts.pool;
        let queued=math::queue_pending(pool_before.queue_usd,pool_before.queue_head,pool_before.queue_tail)?;
        let (mut acid_input,mut gas_share,mut buyback_share)=if root || queued {
            let (acid,gas)=math::loss_split(loss)?;(acid,gas,0)
        } else if native {crate::native_market::loss_split(loss,pool_usd_before,false)?}
          else {math::venue_loss_split_at(loss,pool_usd_before)?};
        // Tiny unswappable allocations stay in the pool, with no reward or burn.
        let small_loss=loss>0 && pump::sell_value(loss,&ctx.accounts.asset,&ctx.accounts.pool_accounts.valuation)?<1000;
        if small_loss {acid_input=loss;gas_share=0;buyback_share=0;}
        let backing = crate::stake_backing::Backing::new(ctx.accounts.pool_accounts.pool.settlement, ctx.accounts.bridge.config.quote_mint);
        let (coverage,mixed_coverage) = {
            sol_quote.validate(Clock::get()?.unix_timestamp)?;
            let current = pump::collateral_value(x.margin, &ctx.accounts.asset, &ctx.accounts.pool_accounts.valuation, sol_quote.price)?;
            let target = if backing.requires(ctx.accounts.asset.key()) {
                crate::stake_backing::loss_shortfall(x.entry_value, current, loss, x.margin)?
            } else { 0 };
            let curve = pump::collateral_curve(&ctx.accounts.settlement, &ctx.accounts.ownership.settlement_valuation, sol_quote.price)?;
            let available = crate::backing_settlement::available(&ctx.accounts.bridge.custody, &trader,&backing)?;
            let plan=crate::backing_basket::coverage(available,&stake_backing::basket_assets(&backing)?,target,&mut |i,n|match i {
                0=>curve.value(n),1=>engine::sol_collateral_value(n,sol_quote.price),_=>Ok(n)
            })?;
            let coverage=crate::stake_backing::Coverage{tokens:plan.debits[0].tokens,quoted_usd:plan.debits[0].quoted_usd,uncovered_usd:plan.uncovered_usd};
            x.collateral_close_value = current;
            x.coverage_tokens = coverage.tokens;
            x.coverage_usd = target - coverage.uncovered_usd;
            // Informational pool shortfall, never personal debt or an unlock lock.
            x.uncovered_usd = coverage.uncovered_usd;
            // Winners keep their stake; only the bet tokens set the profit basis.
            x.close_value = x.entry_value.min(current);
            (coverage,plan)
        };
        #[cfg(feature="mainnet")]
        let profit = if win && !liquidated {
            ctx.accounts.economics.profit(x.opened_at,x.close_value,x.entry,intent.exit,x.leverage,x.long)?
        } else { 0 };
        #[cfg(not(feature="mainnet"))]
        let profit = if win && !liquidated {math::pnl(x.close_value,x.entry,intent.exit,x.leverage,x.long)?.1} else {0};
        #[cfg(feature="mainnet")]
        emit!(ProfitPolicyApplied{position:x.key(),version:if x.opened_at<ctx.accounts.economics.activated_at {0}else{1},
            deadband:if x.opened_at<ctx.accounts.economics.activated_at {true}else{ctx.accounts.economics.deadband},net_profit:profit});
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
        require!(ctx.accounts.pool_accounts.pool.id==0 && fees.gas_asset.mint==anchor_spl::token::spl_token::native_mint::ID, MixError::Binding);
        #[cfg(feature="mainnet")]
        require!(!root || fees.perp.id==102, MixError::Binding);
        require!(fees.main_pool.is_none() && fees.main_inventory.is_none() && fees.main_vault.is_none() && fees.main_ownership_pool.is_none(),MixError::Binding);
        require_keys_eq!(ctx.accounts.settlement.key(),fees.perp.key(),MixError::Binding);
        require_keys_eq!(fees.perp_vault.key(),Pubkey::find_program_address(&[b"vault",ctx.accounts.pool_accounts.pool.key().as_ref(),fees.perp.key().as_ref()],&crate::ID).0,MixError::Binding);
        require_keys_eq!(fees.gas_vault.key(),Pubkey::find_program_address(&[b"gas-vault",ctx.accounts.pool_accounts.pool.key().as_ref()],&crate::ID).0,MixError::Binding);
        if root {
            require!(fees.venue.is_none() && fees.buyback_vault.is_none(),MixError::Binding);
        } else {
            let v=fees.venue.as_ref().ok_or(MixError::Binding)?;
            require!(v.config==ctx.accounts.bridge.config.key() && v.pool==ctx.accounts.pool_accounts.pool.key(),MixError::Binding);
            require_keys_eq!(fees.buyback_vault.as_ref().ok_or(MixError::Binding)?.key(),
                Pubkey::find_program_address(&[b"buyback",ctx.accounts.pool_accounts.pool.key().as_ref()],&crate::ID).0,MixError::Binding);
        }
        let (acid_out,gas_out,buyback_out)=if loss==0 {(0,0,0)} else if !root {
            let route=crate::venue::VenueRoute{pool:&ctx.accounts.pool_accounts.pool,authority:&ctx.accounts.pool_accounts.authority,
                from:&ctx.accounts.asset,perp:&fees.perp,gas:&fees.gas_asset,source:&ctx.accounts.vault,perp_vault:&fees.perp_vault,
                gas_vault:&fees.gas_vault,quote:&ctx.accounts.quote_vault,buyback:fees.buyback_vault.as_ref().ok_or(MixError::Binding)?,
                program:&ctx.accounts.pump_program};
            venue_conversion(&route,accounts,&mut inv.dust,&ctx.accounts.pool_accounts.valuation,&fees.gas_program,
                same,loss,acid_input,gas_share,buyback_share,fee_sell_count,perp_buy_count,min_perp,min_gas)?
        } else {let (acid,gas)=if same {
            // Primary collateral stays primary. Only its 1% gas share is sold.
            require!(perp_buy_count==0,MixError::Binding);
            let gas=if gas_share>0 {
                pump::swap_loss(&ctx.accounts.pool_accounts.pool,&ctx.accounts.pool_accounts.authority,&ctx.accounts.asset,&fees.gas_asset,
                    &ctx.accounts.vault,&fees.gas_vault,&ctx.accounts.quote_vault,&ctx.accounts.pump_program,
                    accounts,fee_sell_count,gas_share,min_gas.max(1))?
            }else{0};
            (acid_input,gas)
        }else if (pump::is_quote(&ctx.accounts.asset) && loss<100) || pump::sell_value(loss,&ctx.accounts.asset,&ctx.accounts.pool_accounts.valuation)?==0 {
            inv.dust=inv.dust.checked_add(loss).ok_or(MixError::Arithmetic)?;
            (0,0)
        }else if ctx.accounts.asset.mint==anchor_spl::token::spl_token::native_mint::ID {
            // Retain native SOL for gas, and convert the other 99% only once.
            require!(perp_buy_count>0 && fee_sell_count as usize+perp_buy_count as usize==accounts.len(),MixError::Binding);
            let bought=pump::swap_loss(&ctx.accounts.pool_accounts.pool,&ctx.accounts.pool_accounts.authority,&ctx.accounts.asset,&fees.perp,
                &ctx.accounts.vault,&fees.perp_vault,&ctx.accounts.quote_vault,&ctx.accounts.pump_program,
                accounts,fee_sell_count,acid_input,min_perp.max(1))?;
            if gas_share>0 {pump::move_tokens(&ctx.accounts.pool_accounts.pool,&ctx.accounts.pool_accounts.authority,&ctx.accounts.vault,&fees.gas_vault,&fees.gas_program.to_account_info(),gas_share)?;}
            (bought,gas_share)
        }else {
            pump::swap_fees(&ctx.accounts.pool_accounts.pool,&ctx.accounts.pool_accounts.authority,&ctx.accounts.asset,&fees.perp,&fees.gas_asset,
                &ctx.accounts.vault,&fees.perp_vault,&fees.gas_vault,&ctx.accounts.quote_vault,&ctx.accounts.pump_program,
                accounts,fee_sell_count,perp_buy_count,loss,gas_share,min_perp,min_gas)?
        };(acid,gas,0)};
        let fees=&mut ctx.accounts.fees;
        let inv = &mut ctx.accounts.inventory;
        let (payout_receipts,staker_out,burn_out)=if small_loss || queued {(acid_out,0,0)}
            else if root {math::acid_split_at(acid_out,pool_usd_before)?}
            else if native {crate::native_market::receipt_split(acid_out,pool_usd_before,false)?}
            else {math::venue_token_split_at(acid_out,pool_usd_before)?};
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
        if let Some(v)=fees.venue.as_mut() {
            v.buyback_received=v.buyback_received.checked_add(buyback_out).ok_or(MixError::Arithmetic)?;
            #[cfg(feature="mainnet")]
            if native {
                let revenue=fees.native_revenue.as_mut().ok_or(MixError::Binding)?;
                require!(revenue.pool==v.pool && revenue.key()==Pubkey::find_program_address(
                    &[b"native-revenue",v.pool.as_ref()],&crate::ID).0,MixError::Binding);
                let (rewards,burn)=crate::native_market::sdice_quote_split(buyback_out,pool_usd_before);
                revenue.rewards_quote=revenue.rewards_quote.checked_add(rewards).ok_or(MixError::Arithmetic)?;
                revenue.burn_quote=revenue.burn_quote.checked_add(burn).ok_or(MixError::Arithmetic)?;
            } else {require!(fees.native_revenue.is_none(),MixError::Binding);}
            emit!(crate::venue::VenueLossSplit{position:x.key(),venue:v.key(),loss,token_tokens:acid_input,gas_tokens:gas_share,
                buyback_tokens:buyback_share,token_bought:acid_out,sol_received:gas_out,buyback_quote:buyback_out});
        }
        // Keep the old event decodable for historical fee-reserve receipts.
        emit!(LossSplit{position:x.key(),loss,market_tokens:acid_input,gas_tokens:gas_share,perp_tokens:0,perp_bought:acid_out,sol_received:gas_out});
        emit!(RevenueSplit{position:x.key(),acid_received:acid_out,pool_tokens:payout_receipts,staker_tokens:staker_out,burned_tokens:burn_out});
        // Preserve actual pool receipts for accounting. Reward the full trader
        // loss plus consumed backing, independent of the revenue allocation.
        let settlement_inventory = if same { &mut ctx.accounts.inventory } else { ctx.accounts.settlement_inventory.as_mut().ok_or(MixError::Binding)? };
        crate::stake_backing::allocate_coverage(&mut ctx.accounts.bridge.custody, &trader, settlement_inventory, coverage, backing.core)?;
        emit!(CoreCoverageApplied { position: x.key(), owner: x.owner, closing_usd: x.collateral_close_value,
            core_tokens: coverage.tokens, covered_usd: x.coverage_usd, uncovered_usd: coverage.uncovered_usd });
        let auxiliary_received=crate::backing_settlement::convert(ctx.accounts,&trader,&mixed_coverage,routes,coverage_accounts)?;
        let x=&mut ctx.accounts.position;
        let settlement_inventory=if same {&mut ctx.accounts.inventory}else{ctx.accounts.settlement_inventory.as_mut().ok_or(MixError::Binding)?};
        settlement_inventory.treasury=settlement_inventory.treasury.checked_add(auxiliary_received).ok_or(MixError::Arithmetic)?;
        let total_coverage=coverage.tokens.checked_add(auxiliary_received).ok_or(MixError::Arithmetic)?;
        x.coverage_tokens=total_coverage;
        let ownership_tokens = payout_receipts.checked_add(total_coverage).ok_or(MixError::Arithmetic)?;
        let tracked_after = pump::spot_value(settlement_inventory.treasury,&ctx.accounts.settlement,&ctx.accounts.ownership.settlement_valuation)? as i128;
        let basis_usd = pump::spot_value(ownership_tokens,&ctx.accounts.settlement,&ctx.accounts.ownership.settlement_valuation)?;
        let advance = (tracked_after-tracked_before).max(0).min(basis_usd as i128) as u64;
        let reward_basis=loss_reward_tokens.checked_add(total_coverage).ok_or(MixError::Arithmetic)?;
        ownership::award_loss(&mut ctx.accounts.ownership.ownership_pool, &mut ctx.accounts.ownership.ownership_trader, &mut ctx.accounts.ownership.ownership_receipt, ownership_tokens,reward_basis,basis_usd,tracked_before,advance,queued)?;
        emit!(PoolTokenomicsApplied { position:x.key(), pool:ctx.accounts.pool_accounts.pool.key(),
            pool_usd_before, queued, reward_factor:emissions::reward_factor(tracked_before) as u64,
            high_pool:pool_usd_before>math::HIGH_POOL_THRESHOLD });
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
        // Include this close's new claim before considering a reward sweep.
        if pool.queue_head == pool.queue_tail {
            ownership::sweep_surplus(&mut ctx.accounts.ownership.ownership_pool,settlement_inventory,
                pool.queue_usd,&ctx.accounts.settlement,&ctx.accounts.ownership.settlement_valuation)?;
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

#[event]
pub struct PoolTokenomicsApplied { pub position:Pubkey, pub pool:Pubkey, pub pool_usd_before:u64,
    pub queued:bool, pub reward_factor:u64, pub high_pool:bool }

/// Created-venue loss conversion: 98% or 90% to the venue token, 1% SOL gas and 1% or 9%
/// swap quote into the SDICE buyback vault. Returns (token, SOL, buyback quote).
#[allow(clippy::too_many_arguments)]
fn venue_conversion<'info>(route:&crate::venue::VenueRoute<'_,'info>,accounts:&[AccountInfo<'info>],dust:&mut u64,
    valuation:&Valuation<'info>,gas_program:&Program<'info,anchor_spl::token::Token>,same:bool,loss:u64,acid_input:u64,gas_share:u64,
    buyback_share:u64,fee_sell_count:u8,perp_buy_count:u8,min_perp:u64,min_gas:u64)->Result<(u64,u64,u64)> {
    if same {
        // The venue token stays the venue token; only its 2% fee share is sold.
        require!(perp_buy_count==0,MixError::Binding);
        if route.from.mint==anchor_spl::token::spl_token::native_mint::ID {
            require!(fee_sell_count==0 && accounts.is_empty(),MixError::Binding);
            if gas_share>0 {pump::move_tokens(route.pool,route.authority,route.source,route.gas_vault,&gas_program.to_account_info(),gas_share)?;}
            if buyback_share>0 {pump::move_tokens(route.pool,route.authority,route.source,route.buyback,&gas_program.to_account_info(),buyback_share)?;}
            return Ok((acid_input,gas_share,buyback_share));
        }
        let fee=gas_share+buyback_share;
        if fee==0 {return Ok((acid_input,0,0));}
        let (_,gas,buyback)=pump::route_venue(route,accounts,fee_sell_count,0,fee,0,gas_share,buyback_share,1,min_gas)?;
        return Ok((acid_input,gas,buyback));
    }
    if (pump::is_quote(route.from) && loss<100) || pump::sell_value(loss,route.from,valuation)?==0 {
        *dust=dust.checked_add(loss).ok_or(MixError::Arithmetic)?;
        return Ok((0,0,0));
    }
    if route.from.mint==anchor_spl::token::spl_token::native_mint::ID {
        // Keep native SOL for gas; convert the remaining 99% once.
        require!(perp_buy_count>0,MixError::Binding);
        if gas_share>0 {pump::move_tokens(route.pool,route.authority,route.source,route.gas_vault,&gas_program.to_account_info(),gas_share)?;}
        let (bought,_,buyback)=pump::route_venue(route,accounts,fee_sell_count,perp_buy_count,acid_input+buyback_share,acid_input,0,buyback_share,min_perp,1)?;
        return Ok((bought,gas_share,buyback));
    }
    pump::route_venue(route,accounts,fee_sell_count,perp_buy_count,loss,acid_input,gas_share,buyback_share,min_perp,min_gas)
}
