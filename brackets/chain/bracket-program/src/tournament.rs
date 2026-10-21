use super::*;

pub fn process(program:&Pubkey,accounts:&[AccountInfo],data:&[u8])->ProgramResult{
    let tag=data[0];let mut it=accounts.iter();let now=Clock::get()?.unix_timestamp;
    if tag==0 {
        check(data.len()==1,9)?;
        let owner=next_account_info(&mut it)?;let cfg=next_account_info(&mut it)?;let mint=next_account_info(&mut it)?;
        let pd=next_account_info(&mut it)?;let system=next_account_info(&mut it)?;
        signer(owner)?;valid_mint(mint)?;
        check(pd.owner==&bpf_loader_upgradeable::id() && *pd.key==Pubkey::find_program_address(&[program.as_ref()],&bpf_loader_upgradeable::id()).0,10)?;
        {let d=pd.try_borrow_data()?;check(d.len()>=45 && d[..4]==3u32.to_le_bytes() && d[12]==1 && &d[13..45]==owner.key.as_ref(),10)?;}
        let(k,b)=Pubkey::find_program_address(&[b"bracket-config-v1"],program);check(*cfg.key==k,3)?;
        create(owner,cfg,system,program,CONFIG_LEN,&[b"bracket-config-v1",&[b]])?;
        write(cfg,&Config{version:1,mint:*mint.key,bump:b})?;
        for game in 0..2u8 {
            let s=next_account_info(&mut it)?;let(k,b)=Pubkey::find_program_address(&[b"bracket-series-v1",&[game]],program);check(*s.key==k,3)?;
            create(owner,s,system,program,SERIES_LEN,&[b"bracket-series-v1",&[game],&[b]])?;
            write(s,&Series{version:1,game,next:0,active:Pubkey::default(),bump:b})?;
        }return Ok(());
    }
    if tag==1 {
        check(data.len()==49,9)?;
        let sponsor=next_account_info(&mut it)?;let cfg=next_account_info(&mut it)?;let s=next_account_info(&mut it)?;
        let a=next_account_info(&mut it)?;let v=next_account_info(&mut it)?;let mint=next_account_info(&mut it)?;
        let source=next_account_info(&mut it)?;let token=next_account_info(&mut it)?;let system=next_account_info(&mut it)?;let network=next_account_info(&mut it)?;
        signer(sponsor)?;valid_mint(mint)?;
        let c:Config=read(program,cfg,CONFIG_LEN)?;
        let(ck,cb)=Pubkey::find_program_address(&[b"bracket-config-v1"],program);
        check(*cfg.key==ck && c.version==1 && c.bump==cb && c.mint==*mint.key,3)?;
        let mut ss=series(program,s)?;let game=data[1];let nonce=u64_at(data,2)?;
        let prize=u64_at(data,10)?;let fee=u64_at(data,18)?;let requested=i64::from_le_bytes(data[26..34].try_into().unwrap());
        let start=if requested==0 {now}else{requested};let best=data[34];let capacity=u16::from_le_bytes(data[35..37].try_into().unwrap());
        let duration=u32::from_le_bytes(data[37..41].try_into().unwrap());let gas=u64_at(data,41)?;
        check(game==ss.game && ss.active==Pubkey::default() && nonce==ss.next,11)?;
        check(prize>=MIN_PRIZE && fee<=prize && prize.checked_add(fee.checked_mul(capacity as u64).ok_or(ProgramError::ArithmeticOverflow)?).is_some(),12)?;
        check(start>=now && start<=now.checked_add(MAX_ADVANCE).ok_or(ProgramError::ArithmeticOverflow)? && (3600..=86400).contains(&duration),13)?;
        check(best_of(best) && (2..=MAX_PLAYERS as u16).contains(&capacity),14)?;
        let rent=Rent::get()?;let oracle=orao::request_fee(network)?+rent.minimum_balance(1500);
        let match_credit=if game==0 {RPS_CREDIT}else{oracle*(best as u64)+STEP_FEE*(best as u64)*12+100_000};
        let minimum=oracle+(capacity as u64)*(match_credit+rent.minimum_balance(MATCH_LEN)+STEP_FEE*4)+100_000;
        check(gas>=minimum && gas<=100_000_000_000,15)?;
        let(k,b)=Pubkey::find_program_address(&[b"bracket-event-v1",&[game],&nonce.to_le_bytes()],program);check(*a.key==k,3)?;
        create(sponsor,a,system,program,EVENT_LEN,&[b"bracket-event-v1",&[game],&nonce.to_le_bytes(),&[b]])?;
        let(vk,vb)=Pubkey::find_program_address(&[b"bracket-vault-v1",a.key.as_ref()],program);check(*v.key==vk && *token.key==spl_token_2022::id(),3)?;
        create(sponsor,v,system,token.key,165,&[b"bracket-vault-v1",a.key.as_ref(),&[vb]])?;
        invoke(&spl_token_2022::instruction::initialize_account3(token.key,v.key,mint.key,a.key)?,&[v.clone(),mint.clone(),token.clone()])?;
        tokens(source,mint.key,sponsor.key,false)?;transfer(source,v,mint,sponsor,token,prize,None)?;
        invoke(&system_instruction::transfer(sponsor.key,a.key,gas),&[sponsor.clone(),a.clone(),system.clone()])?;
        let closes=start.checked_add(ENTRY_WINDOW).ok_or(ProgramError::ArithmeticOverflow)?;
        let e=Event{version:1,game,nonce,sponsor:*sponsor.key,mint:*mint.key,prize,entry_fee:fee,total:prize,starts:start,closes,
            deadline:closes.checked_add(duration as i64).ok_or(ProgramError::ArithmeticOverflow)?,best_of:best,capacity,phase:0,bump:b,match_credit,
            players:vec![],nodes:vec![],semifinal_losers:[EMPTY;2],places:[EMPTY;3],claimed:0,refunded:0,sponsor_refunded:false,
            seed:[0;32],random:[0;64],oracle_floor:0};
        ss.next=ss.next.checked_add(1).ok_or(ProgramError::ArithmeticOverflow)?;ss.active=*a.key;
        write(s,&ss)?;return write(a,&e);
    }
    if tag==2 {
        check(data.len()==33,9)?;
        let player=next_account_info(&mut it)?;let a=next_account_info(&mut it)?;let en=next_account_info(&mut it)?;
        let v=next_account_info(&mut it)?;let mint=next_account_info(&mut it)?;let source=next_account_info(&mut it)?;
        let token=next_account_info(&mut it)?;let system=next_account_info(&mut it)?;
        signer(player)?;let mut e=event(program,a)?;
        check(e.phase==0 && now<e.closes && e.players.len()<(e.capacity as usize) && !e.players.contains(player.key),16)?;
        let session=Pubkey::new_from_array(data[1..33].try_into().unwrap());check(session!=Pubkey::default(),17)?;
        check(*mint.key==e.mint,4)?;valid_mint(mint)?;vault(program,v,a,mint.key)?;
        let(k,b)=Pubkey::find_program_address(&[b"bracket-entry-v1",a.key.as_ref(),player.key.as_ref()],program);check(*en.key==k,3)?;
        create(player,en,system,program,ENTRY_LEN,&[b"bracket-entry-v1",a.key.as_ref(),player.key.as_ref(),&[b]])?;
        if e.entry_fee>0 {tokens(source,mint.key,player.key,false)?;transfer(source,v,mint,player,token,e.entry_fee,None)?;}
        e.total=e.total.checked_add(e.entry_fee).ok_or(ProgramError::ArithmeticOverflow)?;e.players.push(*player.key);
        write(en,&Entry{version:1,event:*a.key,player:*player.key,session,paid:e.entry_fee,refunded:false,bump:b})?;return write(a,&e);
    }
    if tag==3 {
        check(data.len()==33,9)?;
        let caller=next_account_info(&mut it)?;let a=next_account_info(&mut it)?;let payer=next_account_info(&mut it)?;
        let network=next_account_info(&mut it)?;let treasury=next_account_info(&mut it)?;let random=next_account_info(&mut it)?;
        let system=next_account_info(&mut it)?;let oracle=next_account_info(&mut it)?;
        signer(caller)?;let mut e=event(program,a)?;check(e.phase==0 && now>=e.closes && now<e.deadline && e.players.len()>=2,18)?;
        e.seed=hashv(&[b"sdice:bracket:oracle:v1",program.as_ref(),a.key.as_ref(),&data[1..33]]).to_bytes();
        e.oracle_floor=request(program,a,payer,network,treasury,oracle,random,system,&e.seed)?;
        e.phase=1;spend(a,caller,STEP_FEE)?;return write(a,&e);
    }
    if tag==4 {
        check(data.len()==1,9)?;
        let caller=next_account_info(&mut it)?;let a=next_account_info(&mut it)?;let random=next_account_info(&mut it)?;
        let payer=next_account_info(&mut it)?;let system=next_account_info(&mut it)?;
        signer(caller)?;let mut e=event(program,a)?;check(e.phase==1 && now<e.deadline,18)?;
        e.random=orao::read_request(random,&e.seed,&oracle_payer(program,a.key).0)?.ok_or(ProgramError::Custom(19))?;
        recover(program,a,payer,system,e.oracle_floor)?;
        e.nodes=tree(&seeds(&e.random,a.key,e.players.len())?);e.phase=2;
        if e.players.len()==2 {e.nodes[0]=EMPTY;}
        spend(a,caller,STEP_FEE)?;return write(a,&e);
    }
    if tag==12 || tag==14 {
        check(data.len()==1,9)?;
        let caller=next_account_info(&mut it)?;let a=next_account_info(&mut it)?;let s=next_account_info(&mut it)?;
        signer(caller)?;let mut e=event(program,a)?;let mut ss=series(program,s)?;
        check(ss.game==e.game && ss.active==*a.key && e.phase<3,20)?;
        if tag==12 {
            check(e.phase==2 && e.nodes.len()>=4 && resolved(e.nodes[1]) && resolved(e.nodes[0]) && e.nodes[1]!=EMPTY,21)?;
            e.places[0]=e.nodes[1];e.places[2]=e.nodes[0];e.phase=3;
        }else{
            check(now>=e.deadline || e.phase==0 && now>=e.closes && e.players.len()<2 ||
                e.phase==2 && e.nodes[1]==EMPTY,22)?;e.phase=4;
        }
        ss.active=Pubkey::default();spend(a,caller,STEP_FEE)?;write(s,&ss)?;return write(a,&e);
    }
    if tag==13 || tag==15 {
        check(data.len()==2,9)?;
        let caller=next_account_info(&mut it)?;let a=next_account_info(&mut it)?;let v=next_account_info(&mut it)?;
        let mint=next_account_info(&mut it)?;let destination=next_account_info(&mut it)?;let token=next_account_info(&mut it)?;
        signer(caller)?;let mut e=event(program,a)?;check(*mint.key==e.mint,4)?;valid_mint(mint)?;vault(program,v,a,mint.key)?;
        let amount;
        if tag==13 {
            let rank=data[1] as usize;check(e.phase==3 && rank<3 && e.places[rank]!=EMPTY && e.claimed&(1<<rank)==0,23)?;
            let player=e.players.get(e.places[rank] as usize).ok_or(ProgramError::InvalidAccountData)?;
            tokens(destination,mint.key,player,false)?;amount=prizes(e.total,e.places)?[rank];e.claimed|=1<<rank;
        }else{
            check(e.phase==4,23)?;
            if data[1]==255 {check(!e.sponsor_refunded,23)?;tokens(destination,mint.key,&e.sponsor,false)?;amount=e.prize;e.sponsor_refunded=true;}
            else {
                let player=e.players.get(data[1] as usize).ok_or(ProgramError::InvalidInstructionData)?;
                let en=next_account_info(&mut it)?;let mut x=entry(program,en,a.key,player)?;check(!x.refunded,23)?;
                tokens(destination,mint.key,player,false)?;amount=x.paid;x.refunded=true;e.refunded+=1;write(en,&x)?;
            }
        }
        transfer(v,destination,mint,a,token,amount,Some(&[b"bracket-event-v1",&[e.game],&e.nonce.to_le_bytes(),&[e.bump]]))?;
        return write(a,&e);
    }
    if tag==16 {
        check(data.len()==1,9)?;let a=next_account_info(&mut it)?;let sponsor=next_account_info(&mut it)?;
        let e=event(program,a)?;check(e.phase>=3 && *sponsor.key==e.sponsor,23)?;
        // A cancelled seed request can fulfill later. Recover its rebate too.
        if let Some(payer)=it.next(){let system=next_account_info(&mut it)?;recover(program,a,payer,system,e.oracle_floor)?;}
        let n=a.lamports().saturating_sub(Rent::get()?.minimum_balance(EVENT_LEN));if n>0 {spend(a,sponsor,n)?;}return Ok(());
    }
    if tag==18 {
        check(data.len()==9,9)?;
        let donor=next_account_info(&mut it)?;let a=next_account_info(&mut it)?;let v=next_account_info(&mut it)?;
        let mint=next_account_info(&mut it)?;let source=next_account_info(&mut it)?;let token=next_account_info(&mut it)?;
        signer(donor)?;let mut e=event(program,a)?;let n=u64_at(data,1)?;
        // Extra funding is intentionally excluded until per-donor refund receipts exist.
        check(*donor.key==e.sponsor && e.phase<3 && now<e.deadline && n>0 && *mint.key==e.mint,24)?;
        vault(program,v,a,mint.key)?;tokens(source,mint.key,donor.key,false)?;
        e.prize=e.prize.checked_add(n).ok_or(ProgramError::ArithmeticOverflow)?;e.total=e.total.checked_add(n).ok_or(ProgramError::ArithmeticOverflow)?;
        transfer(source,v,mint,donor,token,n,None)?;return write(a,&e);
    }
    Err(ProgramError::InvalidInstructionData)
}
