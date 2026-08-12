const $ = (id) => document.getElementById(id);
let currentScene = '';
function say(text){ $('live').textContent = text; }
function pretty(v){ return JSON.stringify(v, null, 2); }
async function api(path, opts={}){
  const res = await fetch(path, {credentials:'same-origin', headers:{'content-type':'application/json', ...(opts.headers||{})}, ...opts});
  const txt = await res.text(); let data; try{ data = txt ? JSON.parse(txt) : {}; }catch{ data = {raw:txt}; }
  if(res.status === 401){ showLogin(true); throw new Error(data?.error?.message || 'Требуется pairing'); }
  if(!res.ok){ throw new Error(data?.error?.message || data?.message || `HTTP ${res.status}`); }
  return data;
}
function showLogin(show){ $('login').hidden = !show; if(show) $('pairingSecret').focus(); }
function renderDl(el, obj){ el.innerHTML=''; Object.entries(obj||{}).forEach(([k,v])=>{ const dt=document.createElement('dt'); dt.textContent=k; const dd=document.createElement('dd'); dd.textContent=typeof v==='object'?pretty(v):String(v); el.append(dt,dd); }); }
async function checkAuth(){ const s=await api('/api/auth/status').catch(()=>({authenticated:false})); showLogin(!s.authenticated); if(s.authenticated) await refreshAll(); }
$('loginForm').addEventListener('submit', async e=>{ e.preventDefault(); try{ await api('/api/auth/login',{method:'POST',body:JSON.stringify({secret:$('pairingSecret').value})}); showLogin(false); say('Панель подключена'); await refreshAll(); }catch(err){ alert(err.message); }});
async function refreshAll(){ await Promise.allSettled([refreshHealth(), refreshObs(), refreshScenes(), refreshAudio(), refreshStats(), refreshDa(), refreshTwitch(), refreshDonations()]); }
async function refreshHealth(){ try{ const h=await api('/api/health'); renderDl($('health'), h); }catch(e){ renderDl($('health'), {Ошибка:e.message}); }}
async function refreshObs(){ try{ $('obsStatus').textContent=pretty(await api('/api/obs')); }catch(e){ $('obsStatus').textContent=e.message; }}
async function refreshScenes(){ try{ const data=await api('/api/obs/scenes'); const scenes=data.scenes||[]; currentScene=data.currentProgramSceneName||''; $('sceneSelect').innerHTML=''; $('sceneList').innerHTML=''; scenes.forEach(s=>{ const name=s.sceneName; const opt=document.createElement('option'); opt.value=name; opt.textContent=name+(name===currentScene?' — текущая':''); opt.selected=name===currentScene; $('sceneSelect').append(opt); const li=document.createElement('li'); li.textContent=name+(name===currentScene?' — текущая':''); $('sceneList').append(li); }); await refreshSources(); }catch(e){ $('sceneList').innerHTML=`<li>${e.message}</li>`; }}
$('setScene').onclick=async()=>{ const scene=$('sceneSelect').value; try{ await api('/api/obs/scenes/current',{method:'POST',body:JSON.stringify({sceneName:scene})}); currentScene=scene; say('Сцена изменена: '+scene); await refreshScenes(); }catch(e){ alert(e.message); }};
async function refreshSources(){ if(!currentScene) return; const box=$('sources'); box.innerHTML=''; try{ const data=await api('/api/obs/sources?scene='+encodeURIComponent(currentScene)); (data.sceneItems||[]).forEach(it=>{ const div=document.createElement('div'); div.className='source'; const name=it.sourceName; const enabled=!!it.sceneItemEnabled; div.innerHTML=`<h3>${name}</h3><p>Состояние: ${enabled?'видим':'скрыт'}</p>`; const show=document.createElement('button'); show.textContent='Показать'; show.onclick=()=>setSource(name,true); const hide=document.createElement('button'); hide.textContent='Скрыть'; hide.onclick=()=>setSource(name,false); div.append(show, hide); box.append(div); }); }catch(e){ box.textContent=e.message; }}
async function setSource(name, enabled){ try{ await api('/api/obs/source/visibility',{method:'POST',body:JSON.stringify({sceneName:currentScene,sourceName:name,enabled})}); say(`${name}: ${enabled?'показан':'скрыт'}`); await refreshSources(); }catch(e){ alert(e.message); }}
async function refreshAudio(){ const box=$('audio'); box.innerHTML=''; try{ const data=await api('/api/obs/audio'); (data.audio||[]).forEach(a=>{ const row=document.createElement('div'); row.className='audio-row'; const name=a.inputName; const db=Number(a.volumeDb||0).toFixed(1); row.innerHTML=`<h3>${name}</h3><p>Mute: ${a.muted?'да':'нет'}. Громкость: ${db} dB.</p>`; const mute=btn('Mute',()=>audioMute(name,true)); const unmute=btn('Unmute',()=>audioMute(name,false)); const down=btn('-1 dB',()=>audioVol(name,Number(a.volumeDb||0)-1)); const up=btn('+1 dB',()=>audioVol(name,Number(a.volumeDb||0)+1)); const input=document.createElement('input'); input.type='number'; input.step='0.5'; input.value=db; input.setAttribute('aria-label',`Громкость ${name} dB`); const set=btn('Установить dB',()=>audioVol(name,Number(input.value))); row.append(mute,unmute,down,up,input,set); box.append(row); }); }catch(e){ box.textContent=e.message; }}
function btn(text, on){ const b=document.createElement('button'); b.type='button'; b.textContent=text; b.onclick=on; return b; }
async function audioMute(inputName, muted){ try{ await api('/api/obs/audio/mute',{method:'POST',body:JSON.stringify({inputName,muted})}); say(`${inputName}: ${muted?'mute':'unmute'}`); await refreshAudio(); }catch(e){ alert(e.message); }}
async function audioVol(inputName, volumeDb){ try{ await api('/api/obs/audio/volume',{method:'POST',body:JSON.stringify({inputName,volumeDb})}); say(`${inputName}: громкость ${volumeDb} dB`); await refreshAudio(); }catch(e){ alert(e.message); }}
async function cmd(path, msg, confirmText){ if(confirmText && !confirm(confirmText)) return; try{ await api(path,{method:'POST',body:'{}'}); say(msg); }catch(e){ alert(e.message); }}
$('startStream').onclick=()=>cmd('/api/obs/stream/start','Эфир начат');
$('stopStream').onclick=()=>cmd('/api/obs/stream/stop','Эфир остановлен','Остановить активный эфир?');
$('startRecord').onclick=()=>cmd('/api/obs/record/start','Запись начата');
$('stopRecord').onclick=()=>cmd('/api/obs/record/stop','Запись остановлена');
$('pauseRecord').onclick=()=>cmd('/api/obs/record/pause','Запись на паузе');
$('resumeRecord').onclick=()=>cmd('/api/obs/record/resume','Запись продолжена');
async function refreshStats(){ try{ $('stats').textContent=pretty(await api('/api/obs/stats')); }catch(e){ $('stats').textContent=e.message; }}
async function refreshDa(){ try{ renderDl($('daStatus'), await api('/api/donationalerts/status')); }catch(e){ renderDl($('daStatus'), {Ошибка:e.message}); }}
$('daUrlForm').onsubmit=async e=>{ e.preventDefault(); try{ const r=await api('/api/donationalerts/widget-url',{method:'POST',body:JSON.stringify({url:$('daUrl').value})}); $('daUrl').value=''; say('DonationAlerts настроен'); alert(pretty(r)); await refreshDa(); }catch(err){ alert(err.message); }};
$('daReconcile').onclick=()=>cmd('/api/donationalerts/reconcile','DonationAlerts проверен и восстановлен');
$('daRefreshWidget').onclick=()=>cmd('/api/donationalerts/widget/refresh','DonationAlerts widget обновлён');
$('daMute').onclick=()=>api('/api/donationalerts/widget/mute',{method:'POST',body:JSON.stringify({muted:true})}).then(()=>say('DonationAlerts выключен')).catch(e=>alert(e.message));
$('daUnmute').onclick=()=>api('/api/donationalerts/widget/mute',{method:'POST',body:JSON.stringify({muted:false})}).then(()=>say('DonationAlerts включён')).catch(e=>alert(e.message));
$('daSetVolume').onclick=()=>api('/api/donationalerts/widget/volume',{method:'POST',body:JSON.stringify({volumeDb:Number($('daVolume').value)})}).then(()=>say('Громкость DonationAlerts изменена')).catch(e=>alert(e.message));
$('daOauth').onclick=async()=>{ try{ const r=await api('/api/donationalerts/oauth/start',{method:'POST',body:'{}'}); window.open(r.authorize_url,'_blank','noopener'); }catch(e){ alert(e.message); }};
async function refreshDonations(){ try{ const r=await api('/api/donationalerts/recent'); $('donations').innerHTML=''; (r.donations||[]).forEach(d=>{ const li=document.createElement('li'); li.textContent=`${d.username||'Неизвестно'} — ${d.amount||''} ${d.currency||''}. ${d.message||''}`; $('donations').append(li); }); }catch{} }
async function refreshTwitch(){ try{ renderDl($('twitchStatus'), await api('/api/twitch/status')); }catch(e){ renderDl($('twitchStatus'), {Ошибка:e.message}); }}
$('twitchStart').onclick=async()=>{ try{ const r=await api('/api/twitch/device/start',{method:'POST',body:'{}'}); $('twitchDevice').innerHTML=`<p>Откройте <a href="${r.verification_uri||r.verification_uri_complete}" target="_blank" rel="noopener">страницу Twitch Activate</a> и введите код: <strong>${r.user_code||''}</strong></p>`; if(r.verification_uri_complete) window.open(r.verification_uri_complete,'_blank','noopener'); }catch(e){ alert(e.message); }};
$('twitchCheck').onclick=async()=>{ try{ const r=await api('/api/twitch/device/check',{method:'POST',body:'{}'}); $('twitchDevice').textContent=pretty(r); await refreshTwitch(); }catch(e){ alert(e.message); }};
$('twitchChannelForm').onsubmit=async e=>{ e.preventDefault(); const body={}; ['streamTitle','streamGame','streamLang'].forEach(id=>{ const el=$(id); if(el.value.trim()) body[el.name||id]=el.value.trim(); }); if(body.streamTitle){ body.title=body.streamTitle; delete body.streamTitle; } try{ await api('/api/twitch/channel',{method:'POST',body:JSON.stringify(body)}); say('Twitch channel обновлён'); }catch(err){ alert(err.message); }};
$('markerForm').onsubmit=async e=>{ e.preventDefault(); try{ await api('/api/twitch/marker',{method:'POST',body:JSON.stringify({description:$('markerDescription').value})}); say('Twitch marker создан'); }catch(err){ alert(err.message); }};
$('refreshAll').onclick=refreshAll; $('refreshObs').onclick=refreshObs; $('refreshScenes').onclick=refreshScenes; $('refreshSources').onclick=refreshSources; $('refreshAudio').onclick=refreshAudio; $('refreshStats').onclick=refreshStats;
checkAuth();
