//! Print the registry workflow JSON for a DAG name, for integration testing.
//! Usage: cargo run --example dump_dag -- yield_rotation
fn main() {
    let name = std::env::args().nth(1).unwrap_or_else(|| "yield_rotation".into());
    match gateway_rs::dag_registry::workflow_for(&name, Some("wallet-1"), Some("base_sepolia")) {
        Some(wf) => println!("{}", serde_json::to_string(&wf).unwrap()),
        None => {
            eprintln!("unknown dag: {name}");
            std::process::exit(1);
        }
    }
}
