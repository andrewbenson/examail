use exo_store::{
    AccountCalendar, CalendarAccount, CalendarNotifications, CalendarSetting, Settings,
};
use exo_ui::CalendarColor;
use gmail::calendar::CalendarListEntry;

use super::{ListedCalendar, PICKER_ROW, event};

impl crate::Mail {
    /// The calendars for the week view's list: the mail account's, then each other calendar
    /// account's by address, each primary first and then by name. Rooms, weather and deleted
    /// calendars are left out.
    pub(crate) fn calendar_list(&self) -> Vec<ListedCalendar> {
        let data = self.account.calendar().data();
        let email = self.account.email();
        let settings = self.account.settings();
        let colors = event::colors(settings);
        let mut listed: Vec<(bool, ListedCalendar)> = data
            .calendars
            .iter()
            .flatten()
            .filter(|calendar| listable(&calendar.entry))
            .filter_map(
                |AccountCalendar {
                     account,
                     entry: calendar,
                 }| {
                    let id = calendar.id.clone()?;
                    let setting = settings
                        .calendar_accounts
                        .get(account)
                        .and_then(|account| account.calendars.get(&id));
                    let name = calendar
                        .summary_override
                        .clone()
                        .or_else(|| calendar.summary.clone())
                        .unwrap_or_else(|| id.clone());
                    let color = colors.get(&id).copied().unwrap_or(CalendarColor::BLUE);
                    let writable =
                        matches!(calendar.access_role.as_deref(), Some("owner" | "writer"));
                    Some((
                        calendar.primary == Some(true),
                        ListedCalendar {
                            account: account.clone(),
                            shown: setting.is_some_and(|setting| setting.show),
                            id,
                            name,
                            color,
                            writable,
                        },
                    ))
                },
            )
            .collect();
        let key = |account: &str| (account != email, account.to_lowercase());
        listed.sort_by(|(a_primary, a), (b_primary, b)| {
            key(&a.account)
                .cmp(&key(&b.account))
                .then(b_primary.cmp(a_primary))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        listed.into_iter().map(|(_, calendar)| calendar).collect()
    }

    /// Switches on each calendar account's calendars the first time its list comes. For the mail
    /// account that's its primary and the ones Google shows, and for another account its primary
    /// alone, as signing it in does.
    pub(super) fn calendar_enable_by_default(&mut self, calendars: &[AccountCalendar]) {
        let email = self.account.email().to_owned();
        let settings = self.account.settings();
        let mut accounts: Vec<String> = settings
            .calendar_accounts
            .iter()
            .filter(|(_, account)| account.authed && account.calendars.is_empty())
            .map(|(address, _)| address.clone())
            .collect();
        if !settings.calendar_accounts.contains_key(&email) {
            accounts.push(email.clone());
        }
        for account in accounts {
            let theirs: Vec<CalendarListEntry> = calendars
                .iter()
                .filter(|calendar| calendar.account == account)
                .map(|calendar| calendar.entry.clone())
                .collect();
            if theirs.is_empty() {
                continue;
            }
            self.account.update_settings(|settings| {
                let enabled = enable_by_default(settings, &account, &theirs, account != email);
                settings.calendar_accounts.insert(account.clone(), enabled);
            });
        }
    }

    /// A key in a calendar row's colour picker. The calendar takes the colour at once, and Enter or
    /// Escape closes the picker, with Escape putting back the colour it opened with.
    pub(super) fn calendar_picker_key(&mut self, key: exo_ui::PickerKey) {
        let Some((id, start)) = self
            .calendar
            .week
            .as_ref()
            .and_then(|week| week.picker.clone())
        else {
            return;
        };
        let colors: Vec<CalendarColor> = CalendarColor::picker().collect();
        let index = |color: CalendarColor| {
            colors
                .iter()
                .position(|candidate| *candidate == color)
                .unwrap_or(0)
        };
        let current = self
            .calendar_list()
            .into_iter()
            .find(|calendar| calendar.id == id)
            .map_or(index(start), |calendar| index(calendar.color));
        match exo_ui::picker_key(key, current, index(start), colors.len(), PICKER_ROW) {
            exo_ui::PickerMove::Preview(pick) => self.calendar_recolor(&id, colors[pick]),
            exo_ui::PickerMove::Close(pick) => {
                self.calendar_recolor(&id, colors[pick]);
                if let Some(week) = &mut self.calendar.week {
                    week.picker = None;
                }
            }
        }
    }

    /// The calendar account that owns calendar `id`, falling back to the primary mail account.
    pub(super) fn calendar_account(&self, id: &str) -> String {
        let data = self.account.calendar().data();
        let calendars = data.calendars.as_deref().unwrap_or(&[]);
        account_for(calendars, self.account.settings(), id, self.account.email()).to_owned()
    }

    /// The calendar account calendar `id` is read through (the mail account for one no list
    /// has), and Google's colour for it.
    fn calendar_holder(&self, id: &str) -> (String, Option<String>) {
        let data = self.account.calendar().data();
        let calendars = data.calendars.as_deref().unwrap_or(&[]);
        let holder =
            account_for(calendars, self.account.settings(), id, self.account.email()).to_owned();
        let color = calendars
            .iter()
            .find(|calendar| calendar.entry.id.as_deref() == Some(id))
            .and_then(|calendar| calendar.entry.background_color.clone());
        (holder, color)
    }

    pub(super) fn calendar_recolor(&mut self, id: &str, color: CalendarColor) {
        let (holder, _) = self.calendar_holder(id);
        self.account.update_settings(|settings| {
            settings.update_calendar(&holder, id, |setting| {
                setting.color = Some(color.name().to_owned())
            })
        });
    }

    pub(super) fn calendar_toggle(&mut self, id: &str) {
        let (holder, provider) = self.calendar_holder(id);
        self.account.update_settings(|settings| {
            let on = settings
                .calendar_accounts
                .get(&holder)
                .and_then(|account| account.calendars.get(id))
                .is_none_or(|setting| !setting.show);
            let color = settings
                .calendar_accounts
                .get(&holder)
                .and_then(|account| account.calendars.get(id))
                .and_then(|setting| setting.color.clone())
                .unwrap_or_else(|| {
                    provider
                        .as_deref()
                        .and_then(snap)
                        .unwrap_or_else(|| {
                            next_unused(
                                settings
                                    .calendar_accounts
                                    .values()
                                    .flat_map(|account| account.calendars.values()),
                            )
                        })
                        .name()
                        .to_owned()
                });
            settings.update_calendar(&holder, id, |setting| {
                setting.show = on;
                setting.sync = on;
                setting.color.get_or_insert_with(|| color.clone());
            });
        });
    }
}

/// Whether calendar settings list `calendar`: not deleted, not a weather or room calendar.
fn listable(calendar: &CalendarListEntry) -> bool {
    let id = calendar.id.as_deref().unwrap_or_default();
    calendar.deleted != Some(true)
        && !id.contains("weather@group.v.calendar.google.com")
        && !id.contains("@resource.calendar.google.com")
}

/// Calendar account `email`'s calendars switched on the first time its list comes, from its list
/// `calendars`: the primary one, and unless `primary_only` those Google shows, each in the colour
/// nearest Google's (or the first unused one of every account's).
fn enable_by_default(
    settings: &Settings,
    email: &str,
    calendars: &[CalendarListEntry],
    primary_only: bool,
) -> CalendarAccount {
    let mut account = CalendarAccount {
        email: email.to_owned(),
        ..settings
            .calendar_accounts
            .get(email)
            .cloned()
            .unwrap_or_default()
    };
    for calendar in calendars {
        let Some(id) = calendar.id.clone() else {
            continue;
        };
        let primary = calendar.primary == Some(true);
        if !(primary || (!primary_only && listable(calendar) && calendar.selected == Some(true))) {
            continue;
        }
        let others = settings
            .calendar_accounts
            .iter()
            .filter(|(address, _)| *address != email)
            .flat_map(|(_, other)| other.calendars.values());
        let color = calendar
            .background_color
            .as_deref()
            .and_then(snap)
            .unwrap_or_else(|| next_unused(others.chain(account.calendars.values())));
        account.calendars.insert(
            id.clone(),
            CalendarSetting {
                id,
                sync: true,
                show: true,
                color: Some(color.name().to_owned()),
                check_for_scheduling_conflicts: primary,
                notifications: primary.then_some(CalendarNotifications {
                    enabled: true,
                    enabled_desktop: true,
                }),
            },
        );
    }
    account
}

/// The first colour in the picker's order no calendar of any account has, or the least used.
fn next_unused<'a>(calendars: impl Iterator<Item = &'a CalendarSetting> + Clone) -> CalendarColor {
    let used = |color: CalendarColor| {
        calendars
            .clone()
            .filter(|calendar| calendar.color.as_deref() == Some(color.name()))
            .count()
    };
    CalendarColor::picker()
        .min_by_key(|color| used(*color))
        .unwrap_or(CalendarColor::BLUE)
}

/// The calendar account that owns calendar `id`, falling back to `fallback`.
fn account_for<'a>(
    calendars: &'a [AccountCalendar],
    settings: &'a Settings,
    id: &str,
    fallback: &'a str,
) -> &'a str {
    if let Some(calendar) = calendars
        .iter()
        .find(|calendar| calendar.entry.id.as_deref() == Some(id))
    {
        return &calendar.account;
    }
    if let Some((account, _)) = settings
        .calendar_accounts
        .iter()
        .find(|(_, account)| account.calendars.contains_key(id))
    {
        return account;
    }
    fallback
}

/// The calendar colour nearest `hex` by a CIE94-style distance in Lab.
fn snap(hex: &str) -> Option<CalendarColor> {
    let hex = hex.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok();
    let rgb = [channel(0)?, channel(2)?, channel(4)?];
    let target = lab(rgb);
    let base = |color: CalendarColor| {
        let [r, g, b, _] = color.base().to_array();
        lab([r, g, b])
    };
    // The first of equals wins.
    CalendarColor::all()
        .map(|color| (color, distance(target, base(color))))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(color, _)| color)
}

/// `rgb` (sRGB) in CIE Lab, against D65 white.
fn lab([r, g, b]: [u8; 3]) -> [f64; 3] {
    let linear = |v: u8| {
        let v = f64::from(v) / 255.0;
        if v > 0.04045 {
            ((v + 0.055) / 1.055).powf(2.4)
        } else {
            v / 12.92
        }
    };
    let (r, g, b) = (linear(r), linear(g), linear(b));
    let f = |v: f64| {
        if v > 0.008856 {
            v.cbrt()
        } else {
            7.787 * v + 16.0 / 116.0
        }
    };
    let x = f((0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047);
    let y = f(0.2126 * r + 0.7152 * g + 0.0722 * b);
    let z = f((0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883);
    [116.0 * y - 16.0, 500.0 * (x - y), 200.0 * (y - z)]
}

/// The distance between two Lab colours that [`snap`] minimizes.
fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    let (dl, da, db) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
    let (c1, c2) = (a[1].hypot(a[2]), b[1].hypot(b[2]));
    let dc = c1 - c2;
    let dh = (da * da + db * db - dc * dc).max(0.0).sqrt();
    let squared = dl * dl + (dc / (1.0 + 0.045 * c1)).powi(2) + (dh / (1.0 + 0.015 * c1)).powi(2);
    squared.max(0.0).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_colours_snap_to_the_nearest_calendar_colour() {
        // The mock's calendars.
        assert_eq!(snap("#9fe1e7").map(CalendarColor::name), Some("blue"));
        assert_eq!(snap("#42d692").map(CalendarColor::name), Some("green"));
        assert_eq!(snap("#16a765").map(CalendarColor::name), Some("forest"));
    }

    #[test]
    fn snap_rejects_malformed_hex_colours() {
        assert_eq!(snap("9fe1e7"), None, "missing #");
        assert_eq!(snap("#fff"), None, "3-digit shorthand");
        assert_eq!(snap("#9fe1e7ff"), None, "8-digit hex with alpha");
        assert_eq!(snap("#9fe1e7extra"), None, "trailing characters");
        assert_eq!(snap("#+a+b+c"), None, "leading plus signs");
        assert_eq!(snap("#zzzzzz"), None, "non-hex digits");
        assert!(snap("#9FE1E7").is_some(), "uppercase 6-digit hex accepted");
    }

    #[test]
    fn account_for_resolves_secondary_calendar_owner_and_falls_back() {
        let calendars = vec![
            AccountCalendar {
                account: "maya@brightmoor.co".into(),
                entry: CalendarListEntry {
                    id: Some("maya@brightmoor.co".into()),
                    ..CalendarListEntry::default()
                },
            },
            AccountCalendar {
                account: "maya.personal@example.com".into(),
                entry: CalendarListEntry {
                    id: Some("family@group.calendar.google.com".into()),
                    ..CalendarListEntry::default()
                },
            },
        ];
        let mut settings = Settings::default();
        let mut alt = CalendarAccount {
            email: "alt@example.com".into(),
            ..CalendarAccount::default()
        };
        alt.calendars.insert(
            "cached-only".into(),
            CalendarSetting {
                id: "cached-only".into(),
                ..CalendarSetting::default()
            },
        );
        settings
            .calendar_accounts
            .insert("alt@example.com".into(), alt);

        assert_eq!(
            account_for(
                &calendars,
                &settings,
                "family@group.calendar.google.com",
                "maya@brightmoor.co",
            ),
            "maya.personal@example.com"
        );
        assert_eq!(
            account_for(&calendars, &settings, "cached-only", "maya@brightmoor.co"),
            "alt@example.com"
        );
        assert_eq!(
            account_for(&calendars, &settings, "unknown-cal", "maya@brightmoor.co"),
            "maya@brightmoor.co"
        );
    }

    #[test]
    fn the_primary_and_selected_calendars_switch_on() {
        let entry =
            |id: &str, primary: bool, selected: bool, color: Option<&str>| CalendarListEntry {
                id: Some(id.into()),
                primary: primary.then_some(true),
                selected: Some(selected),
                background_color: color.map(Into::into),
                ..CalendarListEntry::default()
            };
        let mut settings = Settings::default();
        let account = enable_by_default(
            &settings,
            "maya@brightmoor.co",
            &[
                entry("maya@brightmoor.co", true, true, Some("#9fe1e7")),
                entry("team", false, true, None),
                entry("other", false, false, None),
                entry("room@resource.calendar.google.com", false, true, None),
            ],
            false,
        );
        let colors: Vec<(&str, Option<&str>)> = account
            .calendars
            .values()
            .map(|calendar| (calendar.id.as_str(), calendar.color.as_deref()))
            .collect();
        assert_eq!(
            colors,
            [("maya@brightmoor.co", Some("blue")), ("team", Some("red"))],
            "unlisted and room calendars stay off; no colour takes the first unused"
        );
        assert!(account.calendars["maya@brightmoor.co"].check_for_scheduling_conflicts);

        // Another account signed in switches its primary on alone, in a colour no account has.
        settings
            .calendar_accounts
            .insert("maya@brightmoor.co".into(), account);
        let other = enable_by_default(
            &settings,
            "maya.lin@example.com",
            &[
                entry("maya.lin@example.com", true, true, None),
                entry("family", false, true, None),
            ],
            true,
        );
        let colors: Vec<(&str, Option<&str>)> = other
            .calendars
            .values()
            .map(|calendar| (calendar.id.as_str(), calendar.color.as_deref()))
            .collect();
        assert_eq!(colors, [("maya.lin@example.com", Some("pink"))]);
    }
}
