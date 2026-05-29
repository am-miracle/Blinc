//! `cn.Card` — surface container with `cn-card` CSS class + shadow.

use blinc_dsl_core::extern_widget;
use blinc_layout::div::ElementBuilder;

/// `cn.Card { children… }` — container surface.
///
/// Body block ⇨ children. The DSL form
///
/// ```dsl,ignore
/// cn.Card {
///     cn.Label("Email")
///     cn.Button("Save")
///     Text("subtle hint")
/// }
/// ```
///
/// is wrapped in a `cn-card`-classed `Div` with the standard cn shadow,
/// flex-column layout, and left-aligned items.
///
/// `blinc_cn::Card` exposes layout-shape builders (`.w()`, `.h()`,
/// `.p{,x,y}()`, `.m()`, `.shadow_{sm,lg}()` etc.) that overlap with
/// the DSL's universal `Div`-style overlay surface and should ride
/// through that path rather than as per-widget props.
///
/// Children-block plumbing reuses the macro's existing `#[children]`
/// support — the `cn.` namespace works the same as bare-name widgets
/// for body blocks. No new grammar work; the dotted call shape
/// composes with the existing `Name(args) { body }` rule.
#[extern_widget(namespace = "cn", name = "Card")]
pub struct CnCard {
    #[children]
    pub children: Vec<Box<dyn ElementBuilder>>,
}

impl ElementBuilder for CnCard {
    fn build(&self, tree: &mut blinc_layout::LayoutTree) -> blinc_layout::LayoutNodeId {
        // Build the empty cn::Card shell first — gives us the
        // `cn-card` class + shadow + flex-col layout. Then attach
        // the DSL-body children directly to the card's layout node.
        //
        // We can't feed `self.children` into `cn::Card::child()`
        // because that API consumes owned values and we only hold
        // shared refs. Manual tree.add_child has the same observable
        // result — every child's `build()` runs once and its node
        // ends up parented to the card.
        let card_node = blinc_cn::Card::new().build(tree);
        for child in &self.children {
            let child_node = child.build(tree);
            tree.add_child(card_node, child_node);
        }
        card_node
    }

    fn render_props(&self) -> blinc_layout::RenderProps {
        // RenderProps is element-local — children don't contribute,
        // so a fresh `cn::Card::new()` carries exactly the visual
        // state this wrapper renders.
        blinc_cn::Card::new().render_props()
    }

    fn children_builders(&self) -> &[Box<dyn ElementBuilder>] {
        &self.children
    }
}
