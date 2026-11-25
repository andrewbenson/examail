//! The `:` emoji and `;` snippet pickers in a new message's subject. The subject looks for the
//! body's triggers in its text up to the caret and shows the body's dropdowns for them. Each
//! dropdown hangs from the subject's line, with its top left corner at the bottom of the line and
//! as far in as the text before the trigger.

use egui::{
    Event, Id, Rect, Ui,
    text::{CCursor, CCursorRange},
    text_edit::TextEditState,
};
use exo_editor::{Pos, Trigger, TriggerKind};
use exo_ui::TextStyle;

use super::draft::Open;

/// The subject's assistant between frames.
#[derive(Clone, Debug, Default)]
pub(crate) struct State {
    /// The trigger at the subject's caret. Its anchor is the trigger character's left edge on the
    /// line.
    pub trigger: Option<Trigger>,
    /// A trigger character was typed since a row was last picked (`editing`). The `;` picker
    /// needs it.
    armed: bool,
    /// Escape closed the dropdown, so it stays closed until a trigger character is typed again.
    dismissed: bool,
    /// Where the caret goes once a picked row has replaced the trigger's text.
    caret: Option<usize>,
    /// The subject's selection as the field last had it, as the characters it starts and ends
    /// at, which `Use Emoji` and `Use Snippet Inline` type at.
    selection: Option<(usize, usize)>,
}

/// Whether this frame's keys type a trigger character. Call it before the field takes the keys.
pub(super) fn typed_trigger(ui: &Ui) -> bool {
    ui.input(|input| input.events.iter().any(|event| matches!(event, Event::Text(text) if text.chars().any(exo_editor::is_trigger_char))))
}

/// Finds the trigger at the subject's caret, after the subject drew with the keyboard. The subject
/// is field `id`, with its text on `line` in `style`, and `typed` says a trigger character was
/// typed this frame.
pub(super) fn update(ui: &Ui, open: &mut Open, id: Id, line: Rect, style: &TextStyle, typed: bool) {
    let ctx = ui.ctx();
    let state = &mut open.popups.subject;
    if let Some(caret) = state.caret.take() {
        exo_ui::focus_field(ctx, id, CCursorRange::one(CCursor::new(caret)));
    }
    if typed {
        state.armed = true;
        state.dismissed = false;
    }
    let range = TextEditState::load(ctx, id).and_then(|edit| edit.cursor.char_range());
    state.selection = range.map(|range| {
        let (primary, secondary) = (
            usize::from(range.primary.index),
            usize::from(range.secondary.index),
        );
        (primary.min(secondary), primary.max(secondary))
    });
    let caret = range
        .filter(|range| range.is_empty())
        .map(|range| usize::from(range.primary.index));
    let subject = &open.record.subject;
    state.trigger = caret.filter(|_| !state.dismissed).and_then(|caret| {
        let before: String = subject.chars().take(caret).collect();
        let (kind, start, query) = exo_editor::find_trigger(&before)?;
        // Emoji show as typed, but a snippet's `;` has to have been typed. A mention shows
        // nothing. The subject finds `+`, `@` and `-`, but unlike the body it never shows their
        // contacts.
        let shown = match kind {
            TriggerKind::Emoji => true,
            TriggerKind::Snippet => state.armed,
            TriggerKind::Contact(_) => false,
        };
        let x = exo_ui::field_x_scrolled(ctx, id, subject, line, style, start)
            .clamp(line.left(), line.right());
        shown.then(|| Trigger {
            kind,
            query,
            start: Pos::new(0, start),
            end: Pos::new(0, caret),
            anchor: Rect::from_x_y_ranges(x..=x, line.y_range()),
        })
    });
}

/// `Use Emoji` in the subject: `:hello` goes in after the selection, with a space before it unless
/// it follows a space or starts the subject, and a space after it unless a space or the end
/// follows. The caret goes after `:hello`, which opens the picker.
pub(super) fn use_emoji(open: &mut Open) {
    let chars: Vec<char> = open.record.subject.chars().collect();
    let at = open
        .popups
        .subject
        .selection
        .map_or(chars.len(), |(_, end)| end)
        .min(chars.len());
    let space_before = at > 0 && chars[at - 1] != ' ';
    let space_after = at < chars.len() && chars[at] != ' ';
    let text = format!(
        "{}:hello{}",
        if space_before { " " } else { "" },
        if space_after { " " } else { "" }
    );
    splice(open, at..at, &text);
    let state = &mut open.popups.subject;
    state.caret = Some(at + usize::from(space_before) + ":hello".len());
    state.dismissed = false;
}

/// `Use Snippet Inline` in the subject: `;` replaces the selection, with a space before it unless
/// it follows whitespace or starts the subject, which opens the picker as a typed `;` does.
pub(super) fn use_snippet(open: &mut Open) {
    let chars: Vec<char> = open.record.subject.chars().collect();
    let (start, end) = open
        .popups
        .subject
        .selection
        .unwrap_or((chars.len(), chars.len()));
    let (start, end) = (start.min(chars.len()), end.min(chars.len()));
    let text = if start == 0 || chars[start - 1].is_whitespace() {
        ";"
    } else {
        " ;"
    };
    splice(open, start..end, text);
    let state = &mut open.popups.subject;
    state.caret = Some(start + text.chars().count());
    state.armed = true;
    state.dismissed = false;
}

/// Puts `text` in the subject in place of the characters `range`.
fn splice(open: &mut Open, range: std::ops::Range<usize>, text: &str) {
    let chars: Vec<char> = open.record.subject.chars().collect();
    open.record.subject = chars[..range.start]
        .iter()
        .copied()
        .chain(text.chars())
        .chain(chars[range.end..].iter().copied())
        .collect();
}

/// Replaces the trigger's text in the subject with `text` and puts the caret after it.
pub(super) fn replace(open: &mut Open, trigger: &Trigger, text: &str) {
    let len = open.record.subject.chars().count();
    let (start, end) = (trigger.start.offset.min(len), trigger.end.offset.min(len));
    splice(open, start..end, text);
    let state = &mut open.popups.subject;
    state.caret = Some(start + text.chars().count());
    state.armed = false;
    state.trigger = None;
}

/// Escape on the subject's dropdown closes the dropdown and keeps the text.
pub(super) fn dismiss(open: &mut Open) {
    open.popups.subject.dismissed = true;
    open.popups.subject.trigger = None;
}

#[cfg(test)]
mod tests {
    use egui::{Color32, Id, RawInput, Rect, pos2, vec2};
    use exo_store::{DraftAction, DraftRecord};
    use exo_ui::{Family, Font, TextStyle};

    use super::*;

    #[test]
    fn trigger_anchor_accounts_for_horizontal_scroll_and_clamps_to_line() {
        let ctx = egui::Context::default();
        exo_ui::install(&ctx).expect("fonts");
        let style = TextStyle::new(Font::new(Family::Sans, 700), 14.0, 20.0);
        let line = Rect::from_min_size(pos2(20.0, 20.0), vec2(120.0, 20.0));
        let id = Id::new("scrolled-subject");
        let mut subject = format!("{} :smile", "long subject prefix ".repeat(8));
        let caret = subject.chars().count();
        let trigger_start = subject.rfind(':').expect("trigger");

        let record = DraftRecord {
            action: DraftAction::Compose,
            from: None,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.clone(),
            body: String::new(),
            quoted_content: None,
            quoted_inlined: false,
            in_reply_to: None,
            in_reply_to_rfc822: None,
            references: Vec::new(),
            rfc822_id: "x@y".into(),
            fingerprint: String::new(),
            attachments: Vec::new(),
            send_later: None,
            reminder: None,
        };
        let mut open = Open::new("draft".into(), None, record);
        exo_ui::focus_field(&ctx, id, CCursorRange::one(CCursor::new(caret)));

        let mut scrolled_offset = 0.0;
        for _ in 0..3 {
            ctx.run_ui(RawInput::default(), |ui| {
                let _ = exo_ui::text_field(
                    ui,
                    id,
                    &mut subject,
                    line,
                    &style,
                    Color32::BLACK,
                    Color32::LIGHT_BLUE,
                );
                open.record.subject.clone_from(&subject);
                scrolled_offset = exo_ui::field_scroll_offset(ui.ctx(), id);
                update(ui, &mut open, id, line, &style, false);
            })
            .drop_without_applying_deltas();
        }

        assert!(scrolled_offset > 0.0, "long subject scrolled horizontally");
        let trigger = open.popups.subject.trigger.expect("emoji trigger found");
        let unscrolled_x = exo_ui::field_x(&ctx, &subject, line, &style, trigger_start);
        assert!(
            unscrolled_x > line.right(),
            "unscrolled trigger would have overflowed past line.right()"
        );
        assert!(
            trigger.anchor.left() >= line.left() && trigger.anchor.left() <= line.right(),
            "scrolled anchor {} stays clamped within {:?}",
            trigger.anchor.left(),
            line.x_range()
        );
    }
}
