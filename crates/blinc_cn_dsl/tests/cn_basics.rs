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

    // `r##"…"##` (two hashes) because the colour-prop hex strings
    // (`"#FFFFFF"` etc.) contain `#` that would otherwise terminate a
    // single-hash raw string mid-source.
    let src = r##"
        view {
            cn.Card {
                cn.Button("Save", variant = "primary", color = "#FFFFFF")
                cn.Badge("New", variant = "success")
                cn.Alert("Heads up", variant = "warning")
                cn.Label("Email", required = true)
                cn.Separator(orientation = "horizontal", bg = "#E5E7EB")
                cn.Spinner(size = "small", color = "#3B82F6", track_color = "#E5E7EB")
            }
        }
    "##;
    dsl.compile_source(src, "cn_basics.blinc")
        .expect("compile cn basics");
}
