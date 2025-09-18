use serde::Serialize;
use std::sync::{Arc, Mutex};
use sysinfo::{CpuRefreshKind, Pid, ProcessRefreshKind, RefreshKind, System};
use tokio::time::{sleep, Duration};

#[derive(Debug, Clone, Default, Serialize)]
pub struct AgentMetrics {
    // CPU del proceso en % (no global)
    pub cpu_pct: f32,
    // Memoria residente del proceso en MB
    pub mem_mb: u64,
}

#[derive(Clone)]
pub struct MetricsHandle {
    inner: Arc<Mutex<AgentMetrics>>,
}

impl MetricsHandle {
    pub fn new() -> Self {
        Self { inner: Arc::new(Mutex::new(AgentMetrics::default())) }
    }

    pub fn get(&self) -> AgentMetrics {
        self.inner.lock().unwrap().clone()
    }

    pub async fn run_sampler(self) {
        let pid = std::process::id();
        let mut sys = System::new_with_specifics(
            RefreshKind::new()
                .with_processes(ProcessRefreshKind::everything())
                .with_cpu(CpuRefreshKind::everything()),
        );
        loop {
            // refrescar CPU y proceso actual
            sys.refresh_cpu();
            // refrescar solo nuestro proceso para reducir costo
            sys.refresh_process(Pid::from_u32(pid));

            let (cpu_pct, mem_mb) = match sys.process(Pid::from_u32(pid)) {
                Some(p) => {
                    let cpu = p.cpu_usage(); // % del proceso
                    // Convertir memoria a MB. sysinfo devuelve bytes en 0.30
                    let mem_bytes = p.memory();
                    let mem_mb = (mem_bytes as f64 / (1024.0 * 1024.0)).round() as u64;
                    (cpu, mem_mb)
                }
                None => (0.0, 0),
            };

            {
                let mut m = self.inner.lock().unwrap();
                m.cpu_pct = cpu_pct;
                m.mem_mb = mem_mb;
            } // liberar el lock antes de await

            sleep(Duration::from_secs(5)).await;
        }
    }
}
