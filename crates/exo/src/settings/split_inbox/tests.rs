use std::sync::Arc;

use exo_commands::Commands;
use exo_store::{Definitions, ListId, SplitChanges, SplitInbox};

use super::*;
use crate::settings::split_template::TemplateAction;

/// The library's role on screen, by name.
fn role(dialogs: &SplitDialogs) -> Option<String> {
    let Screen::Library(state) = &dialogs.screen else {
        panic!("the library is up");
    };
    let roles = dialogs.library()?.roles();
    Some(roles.get(state.role()?)?.section.name.clone())
}

#[test]
fn a_role_named_in_any_case_shows_its_templates_alone() {
    let mut dialogs = SplitDialogs::default();
    dialogs.set_library(crate::splits::load_library());
    dialogs.open_library_at_role("sales");
    assert_eq!(role(&dialogs).as_deref(), Some("Sales"));
    dialogs.open_list();
    dialogs.open_library_at_role("Astronaut");
    assert_eq!(role(&dialogs), None, "no role has that name");
}

fn test_account(name: &str) -> (Account, std::path::PathBuf) {
    let ctx = egui::Context::default();
    let dir = std::env::temp_dir().join(format!(
        "exo-split-settings-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let endpoints = gmail::Endpoints::mock(&"http://127.0.0.1:9/".parse().expect("mock origin"));
    let account = Account::open(
        &dir,
        crate::account::Credentials {
            email: "maya@brightmoor.co".into(),
            endpoints,
            tokens: Arc::new(gmail::StaticToken("dev".into())),
        },
        ctx,
    )
    .expect("account opens");
    (account, dir)
}

#[test]
fn template_primary_and_save_commit_pending_inputs_abort_on_invalid_and_close_when_turning_on() {
    let (mut account, dir) = test_account("template-commit");
    let mut commands = Commands::default();
    let mut dialogs = SplitDialogs::default();
    dialogs.set_library(crate::splits::load_library());

    // Seed an existing disabled News split so Primary routes into `template_save`.
    let (labels, me) = definitions(&account);
    let mut disabled_news = SplitInbox::new(
        "News",
        "",
        Some("news"),
        Some("splitTemplate_news"),
        Definitions {
            labels: &labels,
            me: &me,
        },
    );
    disabled_news.is_disabled = true;
    account.update_settings(|settings| settings.add_split(disabled_news));

    let settings = account.settings().clone();
    assert!(dialogs.open_template(&settings, "splitTemplate_news", Back::Library));
    if let Screen::Template(form, _) = &mut dialogs.screen {
        form.input = "not-valid".into();
    }
    let outcome = dialogs.run(
        Event::Template(TemplateEvent::Action(TemplateAction::Primary)),
        &mut account,
        &mut commands,
        &ListId::Inbox,
        &std::convert::identity,
    );
    assert!(!outcome.close && outcome.show.is_none());
    if let Screen::Template(form, _) = &dialogs.screen {
        assert!(form.error.is_some(), "invalid pending sender aborts save");
    } else {
        panic!("stays on template screen");
    }

    // Valid pending sender commits, enables the disabled split, returns show + close: true.
    if let Screen::Template(form, _) = &mut dialogs.screen {
        form.input = "newsletter.example.com".into();
        form.error = None;
    }
    let outcome = dialogs.run(
        Event::Template(TemplateEvent::Action(TemplateAction::Primary)),
        &mut account,
        &mut commands,
        &ListId::Inbox,
        &std::convert::identity,
    );
    let news = account
        .settings()
        .splits()
        .find(|s| s.kind.as_deref() == Some("news"))
        .expect("news split");
    assert!(!news.is_disabled);
    assert!(
        news.query().contains("from:newsletter.example.com"),
        "pending input was committed: {}",
        news.query()
    );
    assert!(outcome.close, "turning on disabled template closes dialog");
    assert_eq!(outcome.show, Some(ListId::Split(news.id.clone())));

    // VIP picker pending text commits on `template_primary` (new split) and aborts if invalid.
    let settings = account.settings().clone();
    assert!(dialogs.open_template(&settings, "splitTemplate_vip", Back::Library));
    if let Screen::Template(form, _) = &mut dialogs.screen {
        form.picker.set_text("not-valid");
    }
    let outcome = dialogs.run(
        Event::Template(TemplateEvent::Action(TemplateAction::Primary)),
        &mut account,
        &mut commands,
        &ListId::Inbox,
        &std::convert::identity,
    );
    assert!(!outcome.close && outcome.show.is_none());
    if let Screen::Template(form, _) = &dialogs.screen {
        assert!(form.error.is_some(), "invalid pending VIP text aborts");
    } else {
        panic!("stays on VIP template screen");
    }

    if let Screen::Template(form, _) = &mut dialogs.screen {
        form.picker.set_text("boss@vip.example");
    }
    let outcome = dialogs.run(
        Event::Template(TemplateEvent::Action(TemplateAction::Primary)),
        &mut account,
        &mut commands,
        &ListId::Inbox,
        &std::convert::identity,
    );
    let vip = account
        .settings()
        .splits()
        .find(|s| s.kind.as_deref() == Some("vip"))
        .expect("vip split created");
    assert!(
        vip.query().contains("from:boss@vip.example"),
        "{}",
        vip.query()
    );
    assert!(outcome.close);
    assert_eq!(outcome.show, Some(ListId::Split(vip.id.clone())));

    drop(account);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn toggle_refuses_to_re_enable_split_when_combined_query_is_too_long() {
    let (mut account, dir) = test_account("toggle-too-long");
    let mut commands = Commands::default();
    let (labels, me) = definitions(&account);
    let huge_query = (0..220)
        .map(|i| format!("from:sender-{i}@very-long-domain-name-example.org"))
        .collect::<Vec<_>>()
        .join(" OR ");
    let mut split = SplitInbox::new(
        "Huge",
        &huge_query,
        None,
        None,
        Definitions {
            labels: &labels,
            me: &me,
        },
    );
    split.is_disabled = true;
    account.update_settings(|settings| settings.add_split(split.clone()));

    let result = toggle(
        &mut account,
        &mut commands,
        &std::convert::identity,
        &split,
        SplitChanges::default(),
        &ListId::Inbox,
    );
    assert!(result.is_err(), "overlong re-enable is refused");
    assert!(
        account
            .settings()
            .split(&split.id)
            .is_some_and(|s| s.is_disabled),
        "split remains disabled"
    );

    let mut dialogs = SplitDialogs::default();
    dialogs.open_edit(account.settings(), &split.id);
    let outcome = dialogs.run(
        Event::Toggle,
        &mut account,
        &mut commands,
        &ListId::Inbox,
        &std::convert::identity,
    );
    assert!(!outcome.close);
    assert_eq!(
        dialogs.alert.map(|(kind, _)| kind),
        Some(AlertKind::TooLong),
        "shows TooLong alert when re-enabling overlong split"
    );

    drop(account);
    let _ = std::fs::remove_dir_all(&dir);
}
