//! Account settings mutations and Gmail query-length guards for splits: adding, replacing,
//! toggling, deleting, and switching Important and Other on or off.

use exo_commands::{Commands, Toast};
use exo_store::{
    Definitions, ListId, Me, Settings, SplitChanges, SplitEntry, SplitInbox, query_too_long,
};

use super::{Back, Event};
use crate::account::Account;
use crate::settings::Action;
use crate::splits;

/// The account's labels and addresses, for defining splits.
pub(crate) fn definitions(account: &Account) -> (Vec<exo_store::Label>, Vec<String>) {
    (
        account.snapshot().labels.clone(),
        vec![account.email().to_owned()],
    )
}

/// The alert's text for a split that makes the queries too long (title `Sorry!`), its second
/// paragraph indented.
pub(crate) const TOO_LONG: &str = "This makes your Split Inbox queries too long.\n\n       Please remove some splits or reduce the number of contacts in each split.";

/// Adds `split`, saying `Split '{name}' created.` with `Split Inbox Settings`. Returns false,
/// adding nothing, when that would make a split's Gmail search too long.
pub(crate) fn add<A: Clone + 'static>(
    account: &mut Account,
    commands: &mut Commands<A>,
    wrap: &impl Fn(Action) -> A,
    split: &SplitInbox,
) -> bool {
    let (_, me) = definitions(account);
    if too_long(
        account.settings(),
        None,
        split,
        &Me {
            addresses: &me,
            name: None,
        },
    ) {
        return false;
    }
    account.update_settings(|settings| {
        settings.add_split(split.clone());
        settings.engage_splits();
    });
    commands.toast(
        Toast::new(format!("Split '{}' created.", split.name())).action(
            "Split Inbox Settings",
            None,
            wrap(Action::Split(Event::Edit(split.id.clone()))),
        ),
    );
    true
}

/// Replaces `old` with `new`, saying (when `notify`) `Split '{name}' updated.` (`{name} Split
/// updated.` for a built-in one) with `Split Inbox Settings`. Returns false, changing nothing,
/// when that would make a split's Gmail search too long.
pub(crate) fn replace<A: Clone + 'static>(
    account: &mut Account,
    commands: &mut Commands<A>,
    wrap: &impl Fn(Action) -> A,
    old: &SplitInbox,
    new: &SplitInbox,
    notify: bool,
) -> bool {
    let (_, me) = definitions(account);
    if too_long(
        account.settings(),
        Some(old),
        new,
        &Me {
            addresses: &me,
            name: None,
        },
    ) {
        return false;
    }
    account.update_settings(|settings| settings.replace_split(&old.id, new.clone()));
    if !notify {
        return true;
    }
    let text = if new.is_default() {
        format!("{} Split updated.", new.name())
    } else {
        format!("Split '{}' updated.", new.name())
    };
    commands.toast(Toast::new(text).action(
        "Split Inbox Settings",
        None,
        wrap(Action::Split(Event::Edit(new.id.clone()))),
    ));
    true
}

/// Whether `new` (replacing `old`, or added) makes a split's Gmail search too long.
pub(super) fn too_long(
    settings: &Settings,
    old: Option<&SplitInbox>,
    new: &SplitInbox,
    me: &Me<'_>,
) -> bool {
    let mut splits: Vec<SplitInbox> = settings
        .splits()
        .filter(|split| old.is_none_or(|old| old.id != split.id))
        .cloned()
        .collect();
    splits.push(new.clone());
    query_too_long(&splits, new, me)
}

/// Deletes `split` and says so. For a split from a template, the toast offers the template's
/// settings again, going back to the library.
pub(super) fn delete<A: Clone + 'static>(
    account: &mut Account,
    commands: &mut Commands<A>,
    wrap: &impl Fn(Action) -> A,
    split: &SplitInbox,
) {
    account.update_settings(|settings| {
        settings.remove_split(&split.id);
    });
    let mut toast = Toast::new(format!("Split Inbox deleted: {}", split.name()));
    if let Some(template) = &split.template_id {
        toast = toast.action(
            "Split Inbox Settings",
            None,
            wrap(Action::Split(Event::OpenTemplate(
                template.clone(),
                Back::Library,
            ))),
        );
    }
    commands.toast(toast);
}

/// Turns `split` on or off with `changes`, shows it once on (or goes
/// home if it was on screen as it turned off), and says so. Returns `Err(())`, changing nothing,
/// when turning it on would make a split's Gmail search too long.
pub(crate) fn toggle<A: Clone + 'static>(
    account: &mut Account,
    commands: &mut Commands<A>,
    wrap: &impl Fn(Action) -> A,
    split: &SplitInbox,
    changes: SplitChanges,
    current: &ListId,
) -> Result<Option<ListId>, ()> {
    let (labels, me) = definitions(account);
    let toggled = split.update(
        &SplitChanges {
            is_disabled: Some(!split.is_disabled),
            ..changes
        },
        Definitions {
            labels: &labels,
            me: &me,
        },
    );
    if !toggled.is_disabled
        && too_long(
            account.settings(),
            Some(split),
            &toggled,
            &Me {
                addresses: &me,
                name: None,
            },
        )
    {
        return Err(());
    }
    account.update_settings(|settings| {
        settings.replace_split(&split.id, toggled.clone());
        if !toggled.is_disabled {
            settings.engage_splits();
        }
    });
    let state = if toggled.is_disabled { "off" } else { "on" };
    commands.toast(
        Toast::new(format!("Split Inbox turned {state}: {}", toggled.name())).action(
            "Split Inbox Settings",
            None,
            wrap(Action::Split(Event::Edit(toggled.id.clone()))),
        ),
    );
    Ok(if toggled.is_disabled {
        (*current == ListId::Split(split.id.clone())).then(|| splits::home(account.settings()))
    } else {
        Some(ListId::Split(toggled.id))
    })
}

/// Turns Important and Other on or off. The inbox shows its new home.
pub(super) fn toggle_important_other<A: Clone + 'static>(
    account: &mut Account,
    commands: &mut Commands<A>,
    wrap: &impl Fn(Action) -> A,
) -> ListId {
    account.update_settings(|settings| {
        settings.toggle_important_other();
        if !settings.unified_inbox {
            settings.engage_splits();
        }
    });
    let state = if account.settings().unified_inbox {
        "off"
    } else {
        "on"
    };
    // The Important • Other template's settings (Split Inbox Settings without the library).
    let settings = Event::OpenTemplate("splitTemplate_importantOther".to_owned(), Back::List);
    commands.toast(
        Toast::new(format!("Split Inboxes turned {state}: Important • Other")).action(
            "Split Inbox Settings",
            None,
            wrap(Action::Split(settings)),
        ),
    );
    splits::home(account.settings())
}

/// The Active tab's rows: the splits shown as tabs, with Important and Other (or Inbox), in
/// their order.
pub(super) fn active_rows(settings: &Settings) -> Vec<SplitEntry<'_>> {
    settings
        .split_entries()
        .into_iter()
        .filter(|entry| !entry.is_disabled())
        .collect()
}
