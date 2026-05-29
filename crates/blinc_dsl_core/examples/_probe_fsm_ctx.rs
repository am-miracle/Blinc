//! Acceptance probe — `cargo run -p blinc_dsl_core --example _probe_fsm_ctx`.
//!
//! Compiles `counter_dsl.blinc` (FSM with `context { count }` + transition
//! action bodies), dispatches events against the runtime FSM, and reads
//! the mangled context signal to confirm `ctx.count += 1` etc. propagated.

use blinc_dsl_core::BlincDsl;

const SRC: &str = include_str!("counter_dsl.blinc");

fn main() {
    let _ = tracing_subscriber::fmt().try_init();
    let dsl = BlincDsl::new().expect("dsl");
    dsl.install_runtime_bridge();
    match dsl.compile_source(SRC, "counter_dsl.blinc") {
        Ok(syms) => println!("compile: Ok ({} symbols)", syms.len()),
        Err(e) => {
            println!("compile: Err: {e}");
            return;
        }
    }

    let read = || dsl.get_signal_i32("__fsm_ctx_CounterFsm_count");
    println!("count after init: {:?} (expected Some(0))", read());

    blinc_runtime::fsm::dispatch_default("CounterFsm", "Increment");
    println!("count after 1 Increment: {:?} (expected Some(1))", read());

    blinc_runtime::fsm::dispatch_default("CounterFsm", "Increment");
    blinc_runtime::fsm::dispatch_default("CounterFsm", "Increment");
    println!(
        "count after 3 Increments total: {:?} (expected Some(3))",
        read()
    );

    blinc_runtime::fsm::dispatch_default("CounterFsm", "Reset");
    println!("count after Reset: {:?} (expected Some(0))", read());
}
