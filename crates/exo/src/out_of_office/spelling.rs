//! The Out of Office dialog's message editor spelling: spellchecked like compose's body
//! ([`crate::spelling`]), with right-clicks routed through [`Mail::spelling_frame`].

use crate::Mail;

impl Mail {
    pub(crate) fn out_of_office_editor_mut(&mut self) -> Option<&mut exo_editor::Editor> {
        self.out_of_office
            .dialog
            .as_mut()
            .map(|dialog| dialog.editor_mut())
    }

    pub(crate) fn out_of_office_sync_message(&mut self) {
        if let Some(dialog) = self.out_of_office.dialog.as_mut() {
            dialog.form.body = dialog.editor().html();
        }
    }
}
