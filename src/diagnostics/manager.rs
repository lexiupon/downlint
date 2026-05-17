use crate::diagnostics::{Diagnostic, DiagnosticConfig, check_diagnostics};
use crate::resolution::ConnectionGraph;
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};

#[derive(Debug)]
pub enum DiagnosticEvent {
    Refresh(ConnectionGraph),
}

pub async fn run_diagnostics_manager(
    mut rx: mpsc::Receiver<DiagnosticEvent>,
    config: DiagnosticConfig,
    tx: mpsc::Sender<HashMap<PathBuf, Vec<Diagnostic>>>,
) {
    let mut pending: Option<ConnectionGraph> = None;
    let debounce = Duration::from_millis(200);
    let mut deadline: Option<Instant> = None;

    loop {
        tokio::select! {
            maybe_event = rx.recv() => {
                match maybe_event {
                    Some(DiagnosticEvent::Refresh(graph)) => {
                        pending = Some(graph);
                        deadline = Some(Instant::now() + debounce);
                    }
                    None => break,
                }
            }
            _ = async {
                if let Some(deadline) = deadline {
                    tokio::time::sleep_until(deadline).await;
                }
            }, if deadline.is_some() => {
                if let Some(graph) = pending.take() {
                    let diagnostics = check_diagnostics(&graph, &config);
                    let mut grouped = HashMap::new();
                    for diagnostic in diagnostics {
                        grouped.entry(diagnostic.path.clone()).or_insert_with(Vec::new).push(diagnostic);
                    }
                    let _ = tx.send(grouped).await;
                }
                deadline = None;
            }
        }
    }
}
