//! The event form's About editor spelling: spellchecked like compose's body ([`crate::spelling`]),
//! with right-clicks routed through [`Mail::spelling_frame`].

use crate::Mail;

impl Mail {
    pub(crate) fn calendar_about_editor_mut(&mut self) -> Option<&mut exo_editor::Editor> {
        self.calendar.form.as_mut().map(|form| form.about_mut())
    }

    pub(crate) fn calendar_sync_about(&mut self) {
        if let Some(form) = self.calendar.form.as_mut() {
            form.sync_about();
        }
    }
}
