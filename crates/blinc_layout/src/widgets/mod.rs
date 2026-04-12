//! Ready-to-use widgets with built-in styling and behavior
//!
//! This module provides production-ready widgets that work out of the box
//! in the fluent layout API - no `.build()` required!
//!
//! # Widgets
//!
//! - [`button()`] - Clickable button with hover/press states
//! - [`checkbox()`] - Toggle checkbox with label support
//! - [`text_input()`] - Single-line text input with validation
//! - [`text_area()`] - Multi-line text area
//! - [`scroll()`] - Scrollable container with bounce physics
//! - [`code()`] - Code block with syntax highlighting and line numbers
//!
//! # Example
//!
//! ```ignore
//! use blinc_layout::prelude::*;
//!
//! fn my_form(ctx: &Context) -> impl ElementBuilder {
//!     let username = ctx.use_state_for("username", || text_input_state());
//!     let remember = ctx.use_state_keyed("remember", || false);
//!
//!     div().flex_col().gap(16.0)
//!         // Text input - just works!
//!         .child(text_input(&username).placeholder("Username").w(280.0))
//!         // Checkbox - just works!
//!         .child(checkbox(&remember).label("Remember me"))
//!         // Button - just works!
//!         .child(button("Submit").on_click(|_| println!("Submitted!")))
//! }
//! ```

pub mod blockquote;
pub mod button;
pub mod checkbox;
pub mod code;
pub mod cursor;
pub mod gesture;
pub mod hr;
pub mod link;
pub mod list;
pub mod media;
pub mod overlay;
pub mod radio;
pub mod rich_text_editor;
pub mod scroll;
pub mod table;
pub mod text_area;
pub mod text_edit;
pub mod text_input;
pub mod virtual_list;

// Re-export button widget
pub use button::{button, button_with, Button, ButtonConfig, ButtonVisualState};

// Re-export checkbox widget
pub use checkbox::{checkbox, checkbox_labeled, Checkbox, CheckboxBuilder, CheckboxConfig};

// Re-export radio widget
pub use radio::{radio_group, RadioGroup, RadioGroupBuilder, RadioGroupConfig, RadioLayout};

// Re-export text input widget
pub use text_input::{
    // Blur function for click-outside handling
    blur_all_text_inputs,
    // Cursor blink timing utilities
    elapsed_ms,
    has_focused_text_input,
    request_css_reparse,
    // Rebuild/relayout request functions
    request_full_rebuild,
    request_rebuild,
    // Continuous redraw callback for animation scheduler integration
    set_continuous_redraw_callback,
    take_needs_continuous_redraw,
    take_needs_css_reparse,
    take_needs_rebuild,
    take_needs_relayout,
    text_input,
    text_input_state,
    text_input_state_with_placeholder,
    InputConstraints,
    InputType,
    SharedTextInputState,
    TextInput,
    TextInputConfig,
    TextInputState,
    CURSOR_BLINK_INTERVAL_MS,
};

// Re-export text area widget
pub use text_area::{
    text_area, text_area_state, text_area_state_with_placeholder, SharedTextAreaState, TextArea,
    TextAreaConfig, TextAreaState, TextPosition,
};

// Re-export scroll widget
pub use scroll::{
    scroll, scroll_no_bounce, Scroll, ScrollConfig, ScrollDirection, ScrollPhysics,
    ScrollRenderInfo, ScrollbarConfig, ScrollbarRenderInfo, ScrollbarSize, ScrollbarState,
    ScrollbarVisibility, SharedScrollPhysics,
};

// Re-export cursor widget (canvas-based smooth cursor)
pub use cursor::{
    cursor_canvas, cursor_canvas_absolute, cursor_state, CursorAnimation, CursorState,
    SharedCursorState,
};

// Re-export code widget
pub use code::{
    code, code_editor, code_editor_state, code_minimap, pre, Code, CodeConfig, CodeEditor,
    CodeEditorData, SharedCodeEditorState,
};

// Re-export overlay widget
pub use overlay::{
    overlay_events, overlay_manager, BackdropConfig, ContextMenuBuilder, Corner, DialogBuilder,
    DropdownBuilder, ModalBuilder, OverlayAnimation, OverlayConfig, OverlayHandle, OverlayKind,
    OverlayManager, OverlayManagerExt, OverlayPosition, OverlayState, ToastBuilder,
};

// Re-export table widget
pub use table::{
    cell, striped_tr, table, tbody, td, td_text, tfoot, th, th_text, thead, tr, TableBuilder,
    TableCell,
};

// Re-export blockquote widget
pub use blockquote::{blockquote, blockquote_with_config, Blockquote, BlockquoteConfig};

// Re-export horizontal rule widget
pub use hr::{hr, hr_color, hr_thick, hr_with_bg, hr_with_config, HrConfig};

// Re-export link widget
pub use link::{link, open_url, Link, LinkConfig};

// Re-export list widgets
pub use list::{
    li, ol, ol_start, ol_start_with_config, ol_with_config, task_item, task_item_with_config, ul,
    ul_with_config, ListConfig, ListItem, ListMarker, OrderedList, TaskListItem, UnorderedList,
};
