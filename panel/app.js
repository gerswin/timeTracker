const BASE = location.origin;
const $ = (id)=>document.getElementById(id);

function fmtTs(ms){ if(!ms) return '—'; const d=new Date(Number(ms)); return d.toLocaleString(); }
function json(obj){ return JSON.stringify(obj,null,2); }

async function fetchJson(path){
  const res = await fetch(BASE+path, { cache:'no-store' });
  if(!res.ok) throw new Error(path+': '+res.status);
  return await res.json();
}

async function refreshAll(){
  $('origin').textContent = BASE;
  try{
    const st = await fetchJson('/state');
    $('version').textContent = 'Agent v'+st.agent_version;
    $('device_id').textContent = st.device_id;
    $('agent_version').textContent = st.agent_version;
    $('cpu_pct').textContent = st.cpu_pct.toFixed(2);
    $('mem_mb').textContent = st.mem_mb;
    $('input_idle_ms').textContent = st.input_idle_ms;
    $('activity_state').textContent = st.activity_state;
    $('activity_state').className = st.activity_state;
    $('queue_len').textContent = st.queue_len;
    $('last_event_ts').textContent = fmtTs(st.last_event_ts);
    $('last_heartbeat_ts').textContent = fmtTs(st.last_heartbeat_ts);
    $('queue').textContent = json(st.queue_preview);
    $('perms').innerHTML = st.perms && st.perms.unsupported ? 'No aplica' :
      `<span>Accessibility: <b class="${st.perms.accessibility_ok?'ok':'bad'}">${st.perms.accessibility_ok}</b></span>
       <span style="margin-left:12px">Screen Recording: <b class="${st.perms.screen_recording_ok?'ok':'bad'}">${st.perms.screen_recording_ok}</b></span>`;
  }catch(e){ console.error('state', e); }

  try{
    const q = await fetchJson('/queue?limit=10');
    // Mostrar solo top si distinto a state.queue_preview
    $('queue').textContent = json(q.top);
  }catch(e){ console.error('queue', e); }

  try{
    const s = await fetchJson('/debug/sample');
    $('focus').textContent = json({ app_name:s.app_name, title_source:s.title_source, window_title:s.window_title, cg_title:s.cg_title, ax_title:s.ax_title });
  }catch(e){ $('focus').textContent = '—'; }

  $('updated').textContent = 'Actualizado: '+new Date().toLocaleTimeString();
}

document.addEventListener('DOMContentLoaded', ()=>{
  $('btn-refresh-perms').onclick = refreshAll;
  $('btn-prompt-perms').onclick = ()=>fetchJson('/permissions/prompt').then(()=>setTimeout(refreshAll,1500));
  refreshAll();
  setInterval(refreshAll, 2000);
});

