//! Bulk smoke test for the basic-widget catalog. Every wrapper
//! shipped by `register_basics` must compile through one DSL source
//! — catches name-mangling / prop-registry regressions across the
//! whole leaf-widget surface in one go.

use blinc_dsl_core::BlincDsl;

#[test]
fn cn_basics_compile_in_one_view() {
    let _ = tracing_subscriber::fmt::try_init();

    let dsl = BlincDsl::new().expect("dsl init");
    blinc_cn_dsl::register_basics(&dsl).expect("register cn basics");

    let src = r#"
        view {
            cn.Button("Save", variant = "primary")
            cn.Badge("New", variant = "success")
            cn.Alert("Heads up", variant = "warning")
            cn.Label("Email", required = true)
            cn.Separator(orientation = "horizontal")
            cn.Spinner(size = "small")
        }
    "#;
    dsl.compile_source(src, "cn_basics.blinc")
        .expect("compile cn basics");
}
