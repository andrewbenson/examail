//! Calendar accounts other than the mail account: the week sidebar's list of them, and signing
//! one in. The account holds the grants and does the reading
//! ([`crate::account::Account::sign_in_calendar`]). This module is what the calendar's UI asks
//! of the account.

use super::week::{AccountState, ListedAccount, ListedCalendar};
use crate::thread::invite::{Invite, Response};

/// What a calendar sign-in is for, which decides what its outcome does.
#[derive(Clone, Debug)]
pub(crate) enum SignInFor {
    /// From the list's `Sign in with Google` or `Sign in again`, Calendar Settings' `Sign in with
    /// Google`, or the day view's `Add Calendar`, with nothing more to do once signed in.
    List,
    /// From the invite card's `RSVP` alert. Once the account signs in, the answer is sent.
    Invite(Box<Invite>, Response),
}

impl crate::Mail {
    /// The week sidebar's calendar accounts: those with calendars to list, those loading, and the
    /// signed-out ones. The signed-out ones go last, and within each group the mail account comes
    /// first and the others sort by address.
    pub(crate) fn calendar_accounts_listed(&self) -> Vec<ListedAccount> {
        let email = self.account.email();
        let settings = self.account.settings();
        let loading = self.account.calendar().data().loading_accounts.clone();
        let mut calendars = self.calendar_list();
        let mut listed: Vec<ListedAccount> = Vec::new();
        for (address, account) in &settings.calendar_accounts {
            let state = if !account.authed {
                AccountState::SignedOut
            } else if loading.contains(address) {
                AccountState::Loading
            } else {
                let (theirs, rest): (Vec<ListedCalendar>, Vec<ListedCalendar>) = calendars
                    .into_iter()
                    .partition(|calendar| calendar.account == *address);
                calendars = rest;
                if theirs.is_empty() {
                    continue;
                }
                AccountState::Calendars(theirs)
            };
            listed.push(ListedAccount {
                email: address.clone(),
                state,
            });
        }
        let key = |account: &ListedAccount| {
            (
                matches!(account.state, AccountState::SignedOut),
                account.email != email,
                account.email.to_lowercase(),
            )
        };
        listed.sort_by_key(key);
        listed
    }

    /// The week sidebar's calendar accounts as it lists them: each address, its calendars'
    /// names (none while they load), and whether it's signed out (for tests).
    pub fn calendar_account_list(&self) -> Vec<(String, Vec<String>, bool)> {
        self.calendar_accounts_listed()
            .into_iter()
            .map(|account| match account.state {
                AccountState::Calendars(calendars) => (
                    account.email,
                    calendars
                        .into_iter()
                        .map(|calendar| calendar.name)
                        .collect(),
                    false,
                ),
                AccountState::Loading => (account.email, Vec::new(), false),
                AccountState::SignedOut => (account.email, Vec::new(), true),
            })
            .collect()
    }

    /// The account's other calendar accounts, signed in or not.
    fn calendar_others(&self) -> Vec<String> {
        let email = self.account.email();
        self.account
            .settings()
            .calendar_accounts
            .keys()
            .filter(|address| *address != email)
            .cloned()
            .collect()
    }

    /// The account's other calendar accounts that Google signed out.
    fn calendar_signed_out(&self) -> Vec<String> {
        self.account
            .settings()
            .calendar_accounts
            .iter()
            .filter(|(_, account)| !account.authed)
            .map(|(address, _)| address.clone())
            .collect()
    }

    /// The calendar account `invite` was sent to, whose primary calendar answers it. That's the
    /// mail account, unless the only one invited is another calendar account of mine, signed in or
    /// not.
    pub(crate) fn calendar_invite_account(&self, invite: &Invite) -> String {
        invite
            .calendar_account(self.account.email(), &self.calendar_others())
            .to_owned()
    }

    /// The calendar account `invite` was sent to, if it's signed out and has to sign in before the
    /// invite can be answered.
    pub(crate) fn calendar_invite_needs_sign_in(&self, invite: &Invite) -> Option<String> {
        let account = self.calendar_invite_account(invite);
        self.calendar_signed_out()
            .contains(&account)
            .then_some(account)
    }

    /// Starts signing a calendar account in for `purpose`, with `hint` as the account to ask for.
    pub(crate) fn calendar_sign_in(&mut self, hint: Option<&str>, purpose: SignInFor) {
        if self.account.sign_in_calendar(hint) {
            self.calendar.signing_in = Some(purpose);
        } else {
            log::warn!(
                "signing a calendar account in needs Examail's sign-in, which this session wasn't started with"
            );
        }
    }

    /// Once a frame, picks up the outcome of a calendar sign-in and any grants that Google has
    /// refused for the calendar accounts.
    pub(crate) fn calendar_accounts_frame(&mut self) {
        self.account.check_calendar_grants();
        let (others, signed_out) = (self.calendar_others(), self.calendar_signed_out());
        if let Some(thread) = &mut self.thread {
            thread.set_calendar_accounts(others, signed_out);
        }
        let Some(outcome) = self.account.poll_calendar_sign_in() else {
            return;
        };
        let purpose = self.calendar.signing_in.take();
        match (outcome, purpose) {
            (Ok(_), Some(SignInFor::Invite(invite, response))) => {
                self.calendar_close_sign_in_prompt();
                let mut answered = *invite;
                answered.answer(&self.calendar_invite_account(&answered), response);
                self.calendar_respond_to_invite(answered, response, false);
            }
            (Ok(_), _) => {}
            // A failed sign-in from the list shows nothing, whether the user declined or signed
            // in with another account.
            (Err(error), Some(SignInFor::List) | None) => {
                log::info!("a calendar account didn't sign in: {error}")
            }
            (Err(error), Some(SignInFor::Invite(invite, _))) => {
                self.calendar_close_sign_in_prompt();
                log::info!("an invite's calendar account didn't sign in: {error}");
                let email = self.calendar_invite_account(&invite);
                match error {
                    // Declined on Google's screen, or the browser never came back.
                    exo_auth::Error::AccessDenied | exo_auth::Error::TimedOut => {}
                    exo_auth::Error::WrongAccount { .. } => self.pickers.inform(
                        "Wrong email",
                        format!("Please ensure you sign in with {email}"),
                    ),
                    exo_auth::Error::Google(gmail::Error::Http(_)) => self.pickers.inform(
                        "Offline",
                        "Please go online to respond to this invitation.".to_owned(),
                    ),
                    _ => self.pickers.inform(
                        "Sorry!",
                        "Something went wrong. Please try again in a minute.".to_owned(),
                    ),
                }
            }
        }
    }
}
