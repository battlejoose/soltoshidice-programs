// README SVG illustrations. Run from the repository root; no network/game calls.
import { mkdirSync, writeFileSync } from 'node:fs';
const out = new URL('../docs/assets/overview/', import.meta.url);
mkdirSync(out, { recursive: true });
const esc = s => String(s).replaceAll('&','&amp;').replaceAll('<','&lt;').replaceAll('>','&gt;').replaceAll('"','&quot;');
const t=(x,y,s,n=18,c='#fff4d5',a='')=>`<text x="${x}" y="${y}" font-size="${n}" fill="${c}" ${a}>${esc(s)}</text>`;
const r=(x,y,w,h,c,rx=12,stroke='none')=>`<rect x="${x}" y="${y}" width="${w}" height="${h}" rx="${rx}" fill="${c}" stroke="${stroke}"/>`;
const l=(x,y,a,b,c='#65e6c4',extra='')=>`<path d="M${x} ${y}L${a} ${b}" stroke="${c}" stroke-width="3" fill="none" ${extra}/>`;
const dot=(x,y,n=6,c='#152326')=>`<circle cx="${x}" cy="${y}" r="${n}" fill="${c}"/>`;
function die(x,y,n,angle=0){const p={4:[[-18,-18],[-18,18],[18,-18],[18,18]],5:[[-18,-18],[-18,18],[0,0],[18,-18],[18,18]],6:[[-18,-20],[-18,0],[-18,20],[18,-20],[18,0],[18,20]]};return `<g transform="translate(${x} ${y}) rotate(${angle})">${r(-38,-33,76,76,'#314d4b')}${r(-38,-38,76,76,'#fff4d5')}${p[n].map(([a,b])=>dot(a,b)).join('')}</g>`;}
function card(x,y,rank,suit,angle=0){return `<g transform="translate(${x} ${y}) rotate(${angle})">${r(-37,-53,74,106,'#fff4d5',8)}${t(-26,-23,rank,26,'#142327','font-weight="700"')}${t(0,25,suit,42,suit==='♥'?'#e46e71':'#142327','text-anchor="middle"')}</g>`;}
function art(kind,c){
 const base=`<circle cx="140" cy="140" r="98" fill="${c}" opacity=".08"/>`;
 if(kind==='dice')return base+die(103,111,4,-16)+die(183,168,5,12)+die(83,196,6,-8);
 if(kind==='poker')return base+card(106,131,'A','♠',-13)+card(171,136,'K','♥',12)+[0,1,2].map(i=>`<ellipse cx="139" cy="${223-i*7}" rx="30" ry="9" fill="${c}" stroke="#173d36" stroke-width="2"/>`).join('');
 if(kind==='blackjack')return base+card(104,137,'A','♠',-10)+card(173,140,'J','♥',10)+r(100,211,80,35,c,17)+t(140,237,'21',26,'#122125','text-anchor="middle" font-weight="800"');
 if(kind==='slots')return base+r(51,63,178,174,'#293b41',16,c)+t(140,91,'LUCKY BLOCK',17,c,'text-anchor="middle" font-weight="700"')+[76,124,172].map(x=>r(x,110,40,64,'#fff4d5',6)+t(x+20,153,'7',38,'#dc6370','text-anchor="middle" font-weight="800"')).join('')+l(65,142,216,142,c,'opacity=".3"')+r(77,195,124,23,c,10)+l(231,151,246,111,c)+dot(248,105,11,c);
 if(kind==='fights')return base+`<g transform="translate(35 101) rotate(-12 50 45)">${r(12,48,50,63,c,10)}${r(4,0,74,68,c,25)}${r(65,27,24,45,c,12)}</g><g transform="translate(158 95) rotate(12 50 45)">${r(12,48,50,63,'#a98af4',10)}${r(4,0,74,68,'#a98af4',25)}${r(-7,27,24,45,'#a98af4',12)}</g>${t(140,229,'VS',30,'#fff4d5','text-anchor="middle" font-weight="800"')}`;
 if(kind==='rps')return base+`<path d="M69 79L90 62L117 69L132 93L119 115L83 117L64 100Z" fill="${c}"/>${r(157,65,55,68,'#fff4d5',7)}${[83,97,111].map(y=>l(169,y,199,y,'#7f9f9b')).join('')}<g stroke="#b398fb" stroke-width="8" fill="none"><circle cx="112" cy="197" r="13"/><circle cx="149" cy="197" r="13"/><path d="M119 187L162 140M142 187L99 140"/></g>`;
 return base+r(44,72,193,132,'#182b30',12,'#365455')+[107,143,179].map(y=>l(60,y,220,y,'#30484b')).join('')+`<path d="M61 181L88 163L111 174L140 120L166 133L192 102L221 87" fill="none" stroke="${c}" stroke-width="6" stroke-linejoin="round"/>${dot(221,87,7,c)}${r(59,216,78,31,c,6)}${r(146,216,78,31,'#a98af4',6)}${t(98,238,'LONG',16,'#122125','text-anchor="middle" font-weight="700"')}${t(185,238,'SHORT',16,'#122125','text-anchor="middle" font-weight="700"')}`;
}
const games=[
 ['dice','CEE-LO','One shared bank hand. A deterministic result for every bettor.','#ffe452',[['LOCK WAGER','Reserve bank liability'],['ORAO VRF','Replay the dice rules'],['SETTLE','Pay the original wallet']],'Bank program · Solana base layer · shared spectator timeline'],
 ['poker','TEXAS HOLD’EM','Private cards, player-shuffled decks and program-enforced pots.','#65e6c4',[['SHUFFLE','Peers verify proofs'],['PLAY','Commitments + openings'],['AWARD POT','Cash out table chips']],'Poker program · client shuffle verification · Solana betting and custody'],
 ['blackjack','BLACKJACK','Six decks. One shared dealer. Hit, stand, double or split.','#7dbfff',[['FUND + BET','Player table escrow'],['DRAW + PLAY','MagicBlock base-layer VRF'],['RESOLVE','Escrow credit → cash out']],'Bank program · request-bound card draws · no rollup delegation'],
 ['slots','LUCKY BLOCK','Three reels, one payline and an explicit versioned paytable.','#b398fb',[['SPIN','Lock SDICE wager'],['DRAW REELS','MagicBlock base-layer VRF'],['SETTLE','Bank pays the result']],'Bank program · 96% theoretical RTP · deterministic reel mapping'],
 ['fights','STREET FIGHTS','An accepted challenge becomes a verifiable combat sequence.','#ff9d7d',[['CHALLENGE','Match equal stakes'],['RESOLVE','ORAO VRF + combat rules'],['PAY WINNER','Escrow settles on Solana']],'Fight program · separate challenge escrow · shared 3D replay'],
 ['rps','ROCK · PAPER · SCISSORS','Players choose the moves. Commit first; reveal only afterward.','#65e6c4',[['COMMIT','Salted move hash'],['REVEAL','Check both commitments'],['SCORE + PAY','Best-of + timeout rules']],'RPS program · player-choice fairness · no randomness oracle needed'],
 ['perps','SDICE PERPS','Oracle-priced positions with a separate market payout pool.','#d0f589',[['OPEN','Signed + admitted position'],['TRADE','MagicBlock SVM execution'],['PAY PROFIT','Backing token at spot']],'Separate app · delegated execution · Solana commitments and settlement']
];
for(const [id,title,sub,c,steps,note] of games){
 const flow=steps.map(([a,b],i)=>{const x=304+i*259;return r(x,124,235,78,'#1a2d31',11,'#334b4f')+t(x+16,151,a,16,c,'font-weight="700" letter-spacing="1"')+t(x+16,179,b,15,'#d3dfd9')+(i<2?l(x+240,163,x+254,163,c,'marker-end="url(#arrow)"'):'');}).join('');
 const svg=`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1120 280" role="img" aria-labelledby="title desc"><title id="title">${esc(title)}: how it works</title><desc id="desc">${esc(sub)} ${esc(steps.map(x=>x.join(': ')).join('. '))}. ${esc(note)}</desc><defs><marker id="arrow" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="4" markerHeight="4" orient="auto"><path d="M0 0L10 5L0 10Z" fill="${c}"/></marker></defs><g font-family="Segoe UI,Arial,sans-serif">${r(1,1,1118,278,'#101d21',18,'#334b4f')}<path d="M280 25V255" stroke="#334b4f"/>${art(id,c)}${t(304,53,title,29,c,'font-weight="800" letter-spacing="1"')}${t(304,87,sub,18,'#d3dfd9')}${flow}${t(304,244,note,16,'#a8c0ba')}</g></svg>\n`;
 writeFileSync(new URL(`${id}.svg`,out),svg);
}
console.log(`Wrote ${games.length} README illustrations.`);
