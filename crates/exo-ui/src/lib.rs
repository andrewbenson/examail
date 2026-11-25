//! Exo's design system: its themes, text and components, as egui primitives.

mod alert;
mod anchored_list;
mod attachment;
mod avatar;
mod behind;
mod calendar;
mod checkbox;
mod color_picker;
mod contact_list;
mod contact_search;
mod controls;
mod copy_tooltip;
mod date_picker;
mod dialog;
mod dialog_select;
mod emoji;
pub use emoji::picture as paint_emoji;
mod field;
pub mod find;
mod focus_order;
mod fonts;
mod form;
pub mod gradient;
mod hover_intent;
mod icon_button;
mod icons;
mod keycap;
mod label_chip;
mod layout;
mod list_highlight;
mod menu;
mod message_bar;
mod misspelling;
pub mod motion;
mod quick_tips_bar;
mod right_click;
mod scrollbar;
mod shadow;
mod slider;
mod text;
pub mod text_drag;
mod text_selection;
mod theme;
mod thread_status;
mod title;
mod toast;
mod tooltip;

use std::{collections::HashMap, sync::Arc};

use egui::{Id, epaint::FontColorTransferFunction};

pub use alert::{
    ALERT_PARAGRAPH_GAP, ALERT_TEXT, ALERT_TEXT_INK, ALERT_TEXT_WIDTH, Alert, AlertClick, AlertRun,
    PARAGRAPH, paint_radio, wbrise,
};
pub use anchored_list::{
    AnchoredCard, AnchoredHover, AnchoredList, AnchoredListResponse, keep_row_in_view,
    step_list_index,
};
pub use attachment::{
    CARD_MARGIN_BOTTOM, CARD_MARGIN_X, SMALL_CARD_SIZE, SmallAttachmentCard,
    attachment_badge_color, attachment_bottom_row, attachment_grid, paint_attachment_card,
    paint_attachment_progress, small_card_download,
};
pub use avatar::{
    AvatarPhoto, paint_default_avatar, paint_initial_avatar, paint_photo, paint_photo_avatar,
};
pub use behind::Behind;
pub use calendar::{BlockColors, BlockPalette, CalendarColor};
pub use checkbox::{
    CHECKBOX_SIZE, paint_checkbox, paint_checkbox_inks, paint_legacy_checkbox,
    paint_tinted_checkbox,
};
pub use color_picker::{ColorPicker, Picked, PickerKey, PickerMove, picker_key};
pub use contact_list::{
    ContactColumns, ContactListColors, ContactListResponse, ContactListStyle, ContactRow,
    ContactSelectorList, RowAvatar, StackedContactStyle, find_case_insensitive,
    measure_contact_columns, paint_contact_list_box, paint_matched, rfind_case_insensitive,
};
pub use contact_search::{
    ContactAcceptKeys, ContactInputResponse, ContactKeySnapshot, ContactSearchInput,
    ContactSearchOutcome, show_stacked_contact_popup,
};
pub use controls::{
    DIALOG_SWITCH_ROW_HEIGHT, RadioGroup, SortableList, SortableRowMetrics, SwitchRow,
};
pub use copy_tooltip::{CopyClick, CopyTip, CopyTooltip};
pub use date_picker::{
    DATE_PICKER_OFFSET, DateField, DateFieldOutput, DateFieldStyle, DateInput, DatePick,
    DatePicker, format_date, parse_date,
};
pub use dialog::{
    DIALOG_WIDTH, Dialog, DialogFooter, DialogResponse, dialog_highlight, dialog_surface_highlight,
    paint_close, paint_dialog_footer, paint_dialog_literal_footer,
};
pub use dialog_select::{DialogMenuKind, DialogSelect, DialogSelectEvent, DialogSelectState};
pub use field::{
    FieldMenu, field_index, field_index_scrolled, field_scroll_offset, field_x, field_x_scrolled,
    focus_field, take_field_menu, text_field,
};
pub use focus_order::{hold_arrows, list_keys, tab_order, tab_within};
pub use fonts::{Face, Family, Font};
pub use form::{ActionKind, DIALOG_ACTION_HEIGHT, DialogAction, DialogInput};
pub use hover_intent::HoverIntent;
pub use icon_button::IconButton;
pub use icons::{
    icon_rect, icon_size, icon_svg, paint_icon, paint_icon_on, paint_icon_rotated,
    paint_icon_stroked, paint_icon_two_tone, paint_icon_two_tone_on, paint_loading_spinner,
    paint_spinner,
};
pub use keycap::{KEYCAP_ARROW, KEYCAP_BASELINE, KEYCAP_GAP, KeycapColors, Keycaps, keycap_label};
pub use label_chip::{LabelChip, chip_hover};
pub use layout::{pixel_snap, rem, right_pane_width};
pub use list_highlight::{HighlightBar, HighlightState, HighlightTarget, ListHighlight};
pub use menu::{DialogDropdown, Menu, MenuResponse, paint_dialog_dropdown_with};
pub use message_bar::{MESSAGE_BAR_HEIGHT, MessageBar, MessageBarColor, MessageBarRun};
pub use misspelling::{misspelling_top, paint_misspelling};
pub use quick_tips_bar::{QuickTipsBar, TipPart};
pub use right_click::{claim_right_click, link_at, link_box, right_click_claimed};
pub use scrollbar::Scrollbar;
pub use shadow::{BoxShadow, paint_carbon_highlight, paint_fade_in, scroll_shadow};
pub use slider::{SLIDER_SIZE, SliderColors, paint_slider};
pub use text::{TextStyle, WBR, align_baselines, layout, paint_job, paint_text};
pub use text_selection::{Released, TextSelection};
pub use theme::{Palette, Theme, app_theme, palette, set_theme, theme, with_theme};
pub use thread_status::{
    Star, Status, Unread, animated_star, animated_status, paint_failed_status, paint_status,
    paint_thread_status,
};
pub use title::{Title, take_titles, title};
pub use toast::{TOAST_BOTTOM, ToastActionView, ToastKind, ToastResponse, ToastView};
pub use tooltip::{
    ComposeKeysTooltip, DescribedTooltip, DismissableClick, DismissableTooltip, HintTooltip,
    HoverTooltip, KeyTooltip, LineTooltip, LinesTooltip, Placement, ShortcutTooltip, TextTooltip,
    TooltipPart, paint_editor_tooltip, paint_tooltip_box, text_color as tooltip_text_color,
};

use fonts::{FontBook, VerticalMetrics};

type Metrics = Arc<HashMap<Face, VerticalMetrics>>;

fn metrics_id() -> Id {
    Id::new("exo_ui::font_metrics")
}

/// Why the fonts couldn't be loaded.
#[derive(Debug, thiserror::Error)]
pub enum AssetError {
    /// A macOS font Exo draws with (the system UI font, Courier) is missing or unreadable.
    #[error("couldn't read the system font {path}: {source}")]
    SystemFont {
        /// The file.
        path: &'static str,
        /// The underlying error.
        source: std::io::Error,
    },
    /// A font file isn't a font skrifa can parse.
    #[error("couldn't parse {name}: {message}")]
    Parse {
        /// The face's PostScript name.
        name: &'static str,
        /// What went wrong.
        message: String,
    },
}

/// Sets `ctx` up to draw Exo: its fonts and icons, colour emoji from the system's emoji font, text
/// rasterized the way Chromium does on macOS (no hinting, a linear coverage-to-alpha curve), and
/// SVG rendering.
pub fn install(ctx: &egui::Context) -> Result<(), AssetError> {
    let FontBook {
        mut definitions,
        metrics,
        emoji,
    } = FontBook::load()?;
    if emoji.is_some() {
        emoji::register_family(&mut definitions);
    }
    ctx.set_fonts(definitions);
    if let Some(cell) = emoji {
        emoji::install(ctx, cell);
    }
    ctx.data_mut(|data| data.insert_temp::<Metrics>(metrics_id(), Arc::new(metrics)));
    // Calibrated for dark text on light backgrounds; dark themes still need their own check.
    ctx.all_styles_mut(|style| {
        style.visuals.text_options.color_transfer_function = FontColorTransferFunction::Gamma(1.0);
    });
    egui_extras::install_image_loaders(ctx);
    icons::register(ctx);
    Ok(())
}

/// The id of `source` in the window `ctx` is drawing. egui keeps one `Context` for every window
/// (viewport), so what a screen keeps under a fixed id would be shared by two windows drawing at
/// once. Ids of what a window draws go through here instead, salted with its viewport. What
/// belongs to the app, such as a cache of fonts or icons, keeps a plain `Id::new`.
pub fn id(ctx: &egui::Context, source: impl egui::AsId) -> Id {
    Id::new(source).with(ctx.viewport_id())
}

fn metrics(ctx: &egui::Context, face: Face) -> VerticalMetrics {
    ctx.data(|data| data.get_temp::<Metrics>(metrics_id()))
        .and_then(|metrics| metrics.get(&face).copied())
        .expect("exo_ui::install runs before any text is laid out")
}
