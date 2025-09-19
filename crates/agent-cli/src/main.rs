use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use reqwest::blocking::Client;

#[derive(Parser)]
#[command(name = "agent", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    #[command(name = "policy")] Policy(PolicyCmd),
}

#[derive(Parser)]
struct PolicyCmd {
    #[command(subcommand)]
    sub: PolicySub,
}

#[derive(Subcommand)]
enum PolicySub {
    /// Muestra la política efectiva desde el agente local (/state)
    Show {
        /// Solo imprime JSON (policy y etag)
        #[arg(long)]
        json: bool,
    },
    /// Descarga la política desde el backend y la guarda localmente
    Pull,
    /// Abre el panel del agente en el navegador
    Open {
        /// Usa la UI inline (/) en vez del panel estático (/panel)
        #[arg(long)]
        inline: bool,
    },
    /// Aplica una policy local inmediatamente (escribe disco y notifica al agente)
    Apply {
        /// Ruta del archivo JSON con la policy (puede incluir {"policy":{...}} o la policy directa)
        file: String,
    },
    /// Edita el policy.json local con $EDITOR (o abre con app por defecto) y aplica
    Edit,
    /// Solicita al agente que refresque la policy desde el backend (ETag-aware)
    Refresh,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Policy(pc) => match pc.sub {
            PolicySub::Show { json } => policy_show(json),
            PolicySub::Pull => policy_pull(),
            PolicySub::Open { inline } => policy_open(inline),
            PolicySub::Apply { file } => policy_apply(&file),
            PolicySub::Edit => policy_edit(),
            PolicySub::Refresh => policy_refresh(),
        },
    }
}

fn panel_base() -> String {
    std::env::var("PANEL_ADDR").map(|a| format!("http://{}", a)).unwrap_or_else(|_| "http://127.0.0.1:49219".to_string())
}

fn policy_show(json: bool) -> Result<()> {
    let base = panel_base();
    let url = format!("{}/state", base);
    let resp: serde_json::Value = Client::new().get(url).send()?.error_for_status()?.json()?;
    let policy = resp.get("policy").cloned().unwrap_or(serde_json::json!({}));
    let etag = resp.get("policy_etag").cloned().unwrap_or(serde_json::Value::Null);
    if json {
        println!("{}", serde_json::json!({"policy": policy, "etag": etag}));
    } else {
        println!("Policy ETag: {}", etag);
        println!("Policy JSON:\n{}", serde_json::to_string_pretty(&policy)?);
    }
    Ok(())
}

fn policy_pull() -> Result<()> {
    let api = std::env::var("API_BASE_URL").map_err(|_| anyhow!("API_BASE_URL no configurado"))?;
    let user = std::env::var("USER_EMAIL").map_err(|_| anyhow!("USER_EMAIL no configurado"))?;
    let paths = agent_core::paths::Paths::new()?;
    let secrets = agent_core::auth::AgentSecrets::load(&paths)?.ok_or_else(|| anyhow!("Secrets no encontrados; ejecuta primero el agente para bootstrap"))?;
    let url = format!("{}/v1/policy/{}", api.trim_end_matches('/'), urlencoding::encode(&user));
    let client = Client::new();
    let resp = client.get(&url).header("Agent-Token", secrets.agent_token).send()?;
    if resp.status().is_success() {
        let etag = resp.headers().get("etag").and_then(|v| v.to_str().ok()).map(|s| s.to_string());
        let v: serde_json::Value = resp.json()?;
        let pol_v = v.get("policy").cloned().unwrap_or(v);
        // Guardar en policy.json y policy_meta.json
        std::fs::write(paths.policy_file(), serde_json::to_vec_pretty(&pol_v)?)?;
        let meta = serde_json::json!({"etag": etag});
        std::fs::write(paths.policy_meta_file(), serde_json::to_vec_pretty(&meta)?)?;
        println!("[ok] Policy guardada en {} (etag={:?})", paths.policy_file().display(), meta.get("etag"));
        // Hot-apply en el agente local
        let panel = panel_base();
        let apply = Client::new().post(format!("{}/policy/apply", panel)).json(&pol_v).send()?;
        if apply.status().is_success() { println!("[ok] Policy aplicada en agente local"); Ok(()) }
        else { println!("[warn] No se pudo aplicar en agente: {}", apply.status()); Ok(()) }
    } else if resp.status().as_u16() == 304 {
        println!("[ok] Policy sin cambios (304)");
        Ok(())
    } else {
        Err(anyhow!("Fallo al obtener policy: status {}", resp.status()))
    }
}

fn policy_open(inline: bool) -> Result<()> {
    let base = panel_base();
    let url = if inline { format!("{}/", base) } else { format!("{}/panel", base) };
    webbrowser::open(&url).map(|_| ()).map_err(|e| anyhow!("no se pudo abrir navegador: {}", e))
}

fn policy_apply(file: &str) -> Result<()> {
    let txt = std::fs::read_to_string(file)?;
    let mut v: serde_json::Value = serde_json::from_str(&txt)?;
    // permitir envoltura {"policy":{...}}
    if let Some(p) = v.get("policy").cloned() { v = p; }
    // guardar a disco
    let paths = agent_core::paths::Paths::new()?;
    std::fs::write(paths.policy_file(), serde_json::to_vec_pretty(&v)?)?;
    std::fs::write(paths.policy_meta_file(), serde_json::to_vec_pretty(&serde_json::json!({"etag": null}))?)?;
    // notificar al agente local para hot-apply
    let base = panel_base();
    let url = format!("{}/policy/apply", base);
    let resp = Client::new().post(url).json(&v).send()?;
    if resp.status().is_success() { println!("[ok] policy aplicada y guardada"); Ok(()) }
    else { Err(anyhow!("falló aplicar en agente: {}", resp.status())) }
}

fn policy_edit() -> Result<()> {
    let paths = agent_core::paths::Paths::new()?;
    let f = paths.policy_file();
    if !f.exists() { std::fs::write(&f, b"{}")?; }
    if let Ok(editor) = std::env::var("EDITOR") {
        std::process::Command::new(editor).arg(&f).status()?;
    } else {
        webbrowser::open(f.to_str().unwrap_or("")).ok();
    }
    policy_apply(f.to_str().unwrap_or(""))
}

fn policy_refresh() -> Result<()> {
    let base = panel_base();
    let url = format!("{}/policy/refresh", base);
    let resp = Client::new().post(url).send()?;
    if resp.status().is_success() { println!("[ok] refresh solicitado"); Ok(()) } else { Err(anyhow!("falló refresh: {}", resp.status())) }
}
