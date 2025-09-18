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
    const permsSection = $('perms-section');
    if(permsSection){
      if(st.perms && st.perms.unsupported){
        permsSection.style.display = 'none';
      }else{
        permsSection.style.display = '';
        if(st.perms && typeof st.perms.accessibility_ok !== 'undefined'){
          $('perms').innerHTML = `<span>Accessibility: <b class="${st.perms.accessibility_ok?'ok':'bad'}">${st.perms.accessibility_ok}</b></span>
       <span style="margin-left:12px">Screen Recording: <b class="${st.perms.screen_recording_ok?'ok':'bad'}">${st.perms.screen_recording_ok}</b></span>`;
        }else{
          $('perms').textContent = '-';
        }
      }
    }
  }catch(e){ console.error('state', e); }

  try{
    const q = await fetchJson('/queue?limit=10');
    // Mostrar solo top si distinto a state.queue_preview
    $('queue').textContent = json(q.top);
  }catch(e){ console.error('queue', e); }

  try{
    const sample = await fetchJson('/debug/sample');
    if(sample && sample.unsupported){
      $('focus').textContent = 'No disponible en este sistema';
    }else if(sample && sample.error){
      $('focus').textContent = 'Error: '+sample.error;
    }else{
      const details = {
        app_name: 'app_name' in sample ? sample.app_name : null,
        window_title: 'window_title' in sample ? sample.window_title : null,
        title_source: 'title_source' in sample ? sample.title_source : null,
        input_idle_ms: 'input_idle_ms' in sample ? sample.input_idle_ms : null,
      };
      if('win_pid' in sample){
        details.win_pid = sample.win_pid != null ? sample.win_pid : null;
        details.win_thread_id = sample.win_thread_id != null ? sample.win_thread_id : null;
        details.win_hwnd = sample.win_hwnd != null ? sample.win_hwnd : null;
        details.win_root_hwnd = sample.win_root_hwnd != null ? sample.win_root_hwnd : null;
        details.win_class = sample.win_class != null ? sample.win_class : null;
        details.win_process_path = sample.win_process_path != null ? sample.win_process_path : null;
      }
      $('focus').textContent = json(details);
    }
  }catch(e){ $('focus').textContent = '—'; }

  $('updated').textContent = 'Actualizado: '+new Date().toLocaleTimeString();
}

document.addEventListener('DOMContentLoaded', ()=>{
  $('btn-refresh-perms').onclick = refreshAll;
  $('btn-prompt-perms').onclick = ()=>fetchJson('/permissions/prompt').then(()=>setTimeout(refreshAll,1500));
  refreshAll();
  setInterval(refreshAll, 2000);
});



