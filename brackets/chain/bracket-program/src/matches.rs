use super::*;

fn record(e:&mut Event,index:usize,winner:u8,loser:u8)->ProgramResult{
    check(index<e.nodes.len()/2 && [WAITING,PLAYING].contains(&e.nodes[index]) && resolved(winner),25)?;
    e.nodes[index]=winner;
    if index==1 {e.places[1]=loser;}
    if index==2 || index==3 {e.semifinal_losers[index-2]=loser;}
    Ok(())
}

pub fn process(program:&Pubkey,accounts:&[AccountInfo],data:&[u8])->ProgramResult{
    let tag=data[0];let mut it=accounts.iter();let now=Clock::get()?.unix_timestamp;
    let caller=next_account_info(&mut it)?;let a=next_account_info(&mut it)?;let ma=next_account_info(&mut it)?;
    signer(caller)?;let mut e=event(program,a)?;
    if tag==5 {
        check(data.len()==3,9)?;active(&e,now)?;
        let index=u16::from_le_bytes(data[1..3].try_into().unwrap()) as usize;
        check(index<e.nodes.len()/2 && e.nodes[index]==WAITING,25)?;
        let players=if index==0 {
            check(e.nodes.len()>4 && resolved(e.nodes[2]) && resolved(e.nodes[3]),25)?;e.semifinal_losers
        }else{
            let p=[e.nodes[index*2],e.nodes[index*2+1]];check(p.iter().all(|v|resolved(*v)),25)?;p
        };
        if players.contains(&EMPTY){
            record(&mut e,index,if players[0]!=EMPTY {players[0]}else{players[1]},EMPTY)?;
            spend(a,caller,STEP_FEE)?;return write(a,&e);
        }
        let first=next_account_info(&mut it)?;let second=next_account_info(&mut it)?;let system=next_account_info(&mut it)?;
        let entries=[entry(program,first,a.key,&e.players[players[0] as usize])?,entry(program,second,a.key,&e.players[players[1] as usize])?];
        let(k,b)=Pubkey::find_program_address(&[b"bracket-match-v1",a.key.as_ref(),&(index as u16).to_le_bytes()],program);check(*ma.key==k,3)?;
        let rent=Rent::get()?.minimum_balance(MATCH_LEN).saturating_sub(ma.lamports());
        create(caller,ma,system,program,MATCH_LEN,&[b"bracket-match-v1",a.key.as_ref(),&(index as u16).to_le_bytes(),&[b]])?;
        spend(a,caller,rent+STEP_FEE)?;spend(a,ma,e.match_credit)?;
        let m=Match{version:1,event:*a.key,index:index as u16,players,sessions:[entries[0].session,entries[1].session],phase:if e.game==0 {1}else{0},bump:b,
            wins:[0;2],winner:EMPTY,round:0,deadline:now+PHASE_SECONDS,committed:0,revealed:0,hashes:[[0;32];2],choices:[EMPTY;2],transcript:[0;32],
            last_moves:[EMPTY;2],last_winner:EMPTY,seed:[0;32],random:[0;64],oracle_floor:0,first:0,hp:[100;2],strikes:vec![]};
        e.nodes[index]=PLAYING;write(a,&e)?;return write(ma,&m);
    }
    let mut m=game(program,ma,a.key)?;
    if tag==17 {
        check(data.len()==1 && e.phase>=3,9)?;
        let sponsor=next_account_info(&mut it)?;let payer=next_account_info(&mut it)?;let system=next_account_info(&mut it)?;
        check(*sponsor.key==e.sponsor,3)?;recover(program,ma,payer,system,m.oracle_floor)?;
        let n=ma.lamports().saturating_sub(Rent::get()?.minimum_balance(MATCH_LEN));if n>0 {spend(ma,sponsor,n)?;}return Ok(());
    }
    active(&e,now)?;check((m.index as usize)<e.nodes.len()/2 && e.nodes[m.index as usize]==PLAYING,25)?;
    if tag==6 || tag==7 {
        check(data.len()==if tag==6 {38}else{39},9)?;check(e.game==0 && now<m.deadline && m.round<64,26)?;
        let actor=next_account_info(&mut it)?;signer(actor)?;
        let round=u32::from_le_bytes(data[1..5].try_into().unwrap());let side=data[5] as usize;
        check(side<2 && round==m.round,26)?;
        let player=e.players[m.players[side] as usize];check(*actor.key==player || *actor.key==m.sessions[side],27)?;
        let bit=1<<side;
        if tag==6 {
            check(m.phase==1 && m.committed&bit==0,28)?;m.hashes[side]=data[6..38].try_into().unwrap();m.committed|=bit;
            if m.committed==3 {m.phase=2;m.deadline=now+PHASE_SECONDS;}
        }else{
            check(m.phase==2 && m.committed==3 && m.revealed&bit==0 && data[6]<3,28)?;
            let choice=data[6];let salt:[u8;32]=data[7..39].try_into().unwrap();
            check(rps::commitment(program,ma.key,&player,round,choice,&salt)==m.hashes[side],29)?;
            m.choices[side]=choice;m.revealed|=bit;
            if m.revealed==3 {
                let w=rps::winner(m.choices[0],m.choices[1]).ok_or(ProgramError::InvalidInstructionData)?;
                m.last_moves=m.choices;m.last_winner=w;
                m.transcript=hashv(&[b"sdice:bracket:rps:result:v1",&m.transcript,ma.key.as_ref(),&m.round.to_le_bytes(),&m.hashes[0],&m.hashes[1],&m.choices]).to_bytes();
                if w<2 {m.wins[w as usize]+=1;}m.round+=1;
                if w<2 && m.wins[w as usize]>=e.best_of/2+1 {m.winner=w;m.phase=5;}
                else if m.round>=64 {m.winner=EMPTY;m.phase=5;}
                else {m.phase=1;m.deadline=now+PHASE_SECONDS;m.committed=0;m.revealed=0;m.hashes=[[0;32];2];m.choices=[EMPTY;2];}
            }
        }
        // A browser session can pay and sign directly; a distinct relay adds a second signature.
        spend(ma,caller,if caller.key==actor.key {STEP_FEE}else{MOVE_FEE})?;return write(ma,&m);
    }
    if tag==8 {
        check(data.len()==1 && e.game==0 && [1,2].contains(&m.phase) && now>=m.deadline,30)?;
        m.winner=rps::forfeit(if m.phase==1 {m.committed}else{m.revealed});m.phase=5;
        spend(ma,caller,STEP_FEE)?;return write(ma,&m);
    }
    if tag==9 {
        check(data.len()==33 && e.game==1 && m.phase==0 && m.round<e.best_of as u32,31)?;
        let payer=next_account_info(&mut it)?;let network=next_account_info(&mut it)?;let treasury=next_account_info(&mut it)?;
        let random=next_account_info(&mut it)?;let system=next_account_info(&mut it)?;let oracle=next_account_info(&mut it)?;
        m.seed=hashv(&[b"sdice:bracket:fight:v1",program.as_ref(),ma.key.as_ref(),&m.round.to_le_bytes(),&data[1..33]]).to_bytes();
        m.oracle_floor=request(program,ma,payer,network,treasury,oracle,random,system,&m.seed)?;
        m.phase=3;m.hp=[100;2];m.strikes.clear();m.random=[0;64];
        spend(ma,caller,STEP_FEE)?;return write(ma,&m);
    }
    if tag==10 {
        check(data.len()==1 && e.game==1 && [3,4].contains(&m.phase),31)?;
        let random=next_account_info(&mut it)?;let payer=next_account_info(&mut it)?;let system=next_account_info(&mut it)?;
        let vrf=orao::read_request(random,&m.seed,&oracle_payer(program,ma.key).0)?.ok_or(ProgramError::Custom(19))?;
        recover(program,ma,payer,system,m.oracle_floor)?;
        if m.phase==3 {m.random=vrf;m.first=combat::initiative(&vrf,ma.key);m.phase=4;}
        else {check(m.random==vrf,32)?;}
        for _ in 0..8 {
            let index=m.strikes.len() as u8;let roll=combat::dice(&m.random,ma.key,index)?;
            let strike=combat::strike(m.hp,(m.first+index)%2,index,roll)?;m.hp=strike.hp;m.strikes.push(strike);
            if m.hp.contains(&0) {
                let w=if m.hp[0]==0 {1}else{0};m.wins[w]+=1;m.last_winner=w as u8;
                m.transcript=hashv(&[b"sdice:bracket:fight:result:v1",&m.transcript,ma.key.as_ref(),&m.round.to_le_bytes(),&m.random,&[w as u8]]).to_bytes();
                m.round+=1;if m.wins[w]>=e.best_of/2+1 {m.winner=w as u8;m.phase=5;}else{m.phase=0;}break;
            }
        }
        spend(ma,caller,STEP_FEE)?;return write(ma,&m);
    }
    if tag==11 {
        check(data.len()==1 && m.phase==5,33)?;
        let winner=if m.winner<2 {m.players[m.winner as usize]}else{EMPTY};
        let loser=if m.winner<2 {m.players[1-m.winner as usize]}else{EMPTY};
        record(&mut e,m.index as usize,winner,loser)?;
        spend(ma,caller,STEP_FEE)?;
        let n=ma.lamports().saturating_sub(Rent::get()?.minimum_balance(MATCH_LEN));if n>0 {spend(ma,a,n)?;}
        m.phase=6;write(ma,&m)?;return write(a,&e);
    }
    Err(ProgramError::InvalidInstructionData)
}
