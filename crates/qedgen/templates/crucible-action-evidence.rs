// Process-wide action counters survive fixture resets. The first outcome and
// every 1024th outcome produce cumulative lower bounds, never a full-run claim.
fn record_action_outcome(
    handler: &str,
    counters: &std::sync::Mutex<(u64, u64, u64)>,
    success: bool,
) {
    let mut counts = counters.lock().unwrap_or_else(|error| error.into_inner());
    counts.0 = counts.0.saturating_add(1);
    if success {
        counts.1 = counts.1.saturating_add(1);
    } else {
        counts.2 = counts.2.saturating_add(1);
    }
    if counts.0 == 1 || counts.0 % 1024 == 0 {
        eprintln!(
            "QEDGEN_CRUCIBLE_ACTION_V1 {{\"process_id\":{},\"handler\":\"{}\",\"attempted\":{},\"succeeded\":{},\"rejected\":{}}}",
            std::process::id(), handler, counts.0, counts.1, counts.2
        );
    }
}
