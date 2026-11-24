//! RFC 5545 / RFC 6868 iCalendar content-line helpers and CLDR Windows↔IANA time zone mapping.

use std::collections::BTreeMap;

use jiff::tz::TimeZone;

/// Unfolds RFC 5545 §3.1 folded lines (CRLF or LF followed by a single space or tab continues the
/// prior line) and skips empty lines.
#[must_use]
pub fn unfold(raw: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in raw.split('\n') {
        let line = line.trim_end_matches('\r');
        match line.strip_prefix([' ', '\t']) {
            Some(rest) => {
                if let Some(last) = lines.last_mut() {
                    last.push_str(rest);
                }
            }
            None if !line.is_empty() => lines.push(line.to_owned()),
            None => {}
        }
    }
    lines
}

/// Splits an unfolded RFC 5545 content line into `(NAME, params, value)`:
///
/// - Property name and parameter keys are upper-cased (ASCII).
/// - Semicolons and colons inside `"..."` parameter values are preserved.
/// - Quoted parameter values are unquoted and decoded for RFC 6868 `^`-escapes.
/// - Returns `None` if there is no unquoted `:` or the property name is empty.
#[must_use]
pub fn content_line(line: &str) -> Option<(String, BTreeMap<String, String>, &str)> {
    let mut quoted = false;
    let mut parts = Vec::new();
    let mut start = 0;
    for (at, ch) in line.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            ';' | ':' if !quoted => {
                parts.push(&line[start..at]);
                start = at + 1;
                if ch == ':' {
                    let name = parts.remove(0).to_ascii_uppercase();
                    if name.is_empty() {
                        return None;
                    }
                    let params = parts
                        .into_iter()
                        .filter_map(|param| param.split_once('='))
                        .map(|(key, value)| {
                            let unquoted = value
                                .strip_prefix('"')
                                .and_then(|inner| inner.strip_suffix('"'))
                                .unwrap_or(value);
                            (key.to_ascii_uppercase(), decode_param_value(unquoted))
                        })
                        .collect();
                    return Some((name, params, &line[start..]));
                }
            }
            _ => {}
        }
    }
    None
}

/// Decodes RFC 6868 parameter value escapes (`^n` → newline, `^'` → `"`, `^^` → `^`).
#[must_use]
pub fn decode_param_value(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch == '^' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('\'') => out.push('"'),
                Some('^') => out.push('^'),
                Some(other) => {
                    out.push('^');
                    out.push(other);
                }
                None => out.push('^'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Single-pass RFC 5545 §3.3.11 `TEXT` value unescaping (`\n`/`\N`, `\\`, `\,`, `\;`), so `\\n`
/// becomes literal `\n` rather than a newline.
#[must_use]
pub fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some('\\') => out.push('\\'),
                Some(',') => out.push(','),
                Some(';') => out.push(';'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Trims whitespace and strips a single leading `mailto:` scheme (ASCII case-insensitive).
#[must_use]
pub fn strip_mailto(raw: &str) -> &str {
    let trimmed = raw.trim();
    if trimmed
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("mailto:"))
    {
        trimmed[7..].trim()
    } else {
        trimmed
    }
}

/// Standard Microsoft Windows / Exchange / Outlook time zone names mapped to canonical IANA zone
/// identifiers (Unicode CLDR `windowsZones`).
///
/// Canonical standard names precede aliases that share the same IANA target (for example `"UTC"`
/// precedes `"Coordinated Universal Time"`, and `"Pacific Standard Time"` precedes
/// `"Pacific Daylight Time"`), so [`iana_to_windows`] returns the primary Windows zone name.
pub const WINDOWS_ZONES: &[(&str, &str)] = &[
    ("UTC", "UTC"),
    ("Coordinated Universal Time", "UTC"),
    ("UTC-11", "Etc/GMT+11"),
    ("Dateline Standard Time", "Etc/GMT+12"),
    ("UTC-09", "Etc/GMT+9"),
    ("UTC-08", "Etc/GMT+8"),
    ("UTC-02", "Etc/GMT+2"),
    ("UTC+12", "Etc/GMT-12"),
    ("UTC+13", "Etc/GMT-13"),
    ("Hawaiian Standard Time", "Pacific/Honolulu"),
    ("Aleutian Standard Time", "America/Adak"),
    ("Marquesas Standard Time", "Pacific/Marquesas"),
    ("Alaskan Standard Time", "America/Anchorage"),
    ("Pacific Standard Time", "America/Los_Angeles"),
    ("Pacific Daylight Time", "America/Los_Angeles"),
    ("Pacific Standard Time (Mexico)", "America/Tijuana"),
    ("US Mountain Standard Time", "America/Phoenix"),
    ("Mountain Standard Time", "America/Denver"),
    ("Mountain Daylight Time", "America/Denver"),
    ("Mountain Standard Time (Mexico)", "America/Mazatlan"),
    ("Yukon Standard Time", "America/Whitehorse"),
    ("Central America Standard Time", "America/Guatemala"),
    ("Central Standard Time", "America/Chicago"),
    ("Central Daylight Time", "America/Chicago"),
    ("Central Standard Time (Mexico)", "America/Mexico_City"),
    ("Canada Central Standard Time", "America/Regina"),
    ("Easter Island Standard Time", "Pacific/Easter"),
    ("SA Pacific Standard Time", "America/Bogota"),
    ("Eastern Standard Time", "America/New_York"),
    ("Eastern Daylight Time", "America/New_York"),
    ("Eastern Standard Time (Mexico)", "America/Cancun"),
    ("Haiti Standard Time", "America/Port-au-Prince"),
    ("Cuba Standard Time", "America/Havana"),
    ("US Eastern Standard Time", "America/Indiana/Indianapolis"),
    ("Turks And Caicos Standard Time", "America/Grand_Turk"),
    ("Paraguay Standard Time", "America/Asuncion"),
    ("Atlantic Standard Time", "America/Halifax"),
    ("Venezuela Standard Time", "America/Caracas"),
    ("Central Brazilian Standard Time", "America/Cuiaba"),
    ("SA Western Standard Time", "America/La_Paz"),
    ("Pacific SA Standard Time", "America/Santiago"),
    ("Newfoundland Standard Time", "America/St_Johns"),
    ("Tocantins Standard Time", "America/Araguaina"),
    ("E. South America Standard Time", "America/Sao_Paulo"),
    ("SA Eastern Standard Time", "America/Cayenne"),
    ("Argentina Standard Time", "America/Argentina/Buenos_Aires"),
    ("Greenland Standard Time", "America/Nuuk"),
    ("Montevideo Standard Time", "America/Montevideo"),
    ("Magallanes Standard Time", "America/Punta_Arenas"),
    ("Saint Pierre Standard Time", "America/Miquelon"),
    ("Bahia Standard Time", "America/Bahia"),
    ("Azores Standard Time", "Atlantic/Azores"),
    ("Cape Verde Standard Time", "Atlantic/Cape_Verde"),
    ("GMT Standard Time", "Europe/London"),
    ("Greenwich Standard Time", "Atlantic/Reykjavik"),
    ("Sao Tome Standard Time", "Africa/Sao_Tome"),
    ("Morocco Standard Time", "Africa/Casablanca"),
    ("W. Europe Standard Time", "Europe/Berlin"),
    ("Central Europe Standard Time", "Europe/Budapest"),
    ("Romance Standard Time", "Europe/Paris"),
    ("Central European Standard Time", "Europe/Warsaw"),
    ("W. Central Africa Standard Time", "Africa/Lagos"),
    ("Jordan Standard Time", "Asia/Amman"),
    ("GTB Standard Time", "Europe/Bucharest"),
    ("Middle East Standard Time", "Asia/Beirut"),
    ("Egypt Standard Time", "Africa/Cairo"),
    ("E. Europe Standard Time", "Europe/Chisinau"),
    ("Syria Standard Time", "Asia/Damascus"),
    ("West Bank Standard Time", "Asia/Hebron"),
    ("South Africa Standard Time", "Africa/Johannesburg"),
    ("FLE Standard Time", "Europe/Kyiv"),
    ("Israel Standard Time", "Asia/Jerusalem"),
    ("South Sudan Standard Time", "Africa/Juba"),
    ("Kaliningrad Standard Time", "Europe/Kaliningrad"),
    ("Sudan Standard Time", "Africa/Khartoum"),
    ("Libya Standard Time", "Africa/Tripoli"),
    ("Namibia Standard Time", "Africa/Windhoek"),
    ("Arabic Standard Time", "Asia/Baghdad"),
    ("Turkey Standard Time", "Europe/Istanbul"),
    ("Arab Standard Time", "Asia/Riyadh"),
    ("Belarus Standard Time", "Europe/Minsk"),
    ("Russian Standard Time", "Europe/Moscow"),
    ("E. Africa Standard Time", "Africa/Nairobi"),
    ("Iran Standard Time", "Asia/Tehran"),
    ("Arabian Standard Time", "Asia/Dubai"),
    ("Astrakhan Standard Time", "Europe/Astrakhan"),
    ("Azerbaijan Standard Time", "Asia/Baku"),
    ("Russia Time Zone 3", "Europe/Samara"),
    ("Mauritius Standard Time", "Indian/Mauritius"),
    ("Saratov Standard Time", "Europe/Saratov"),
    ("Georgian Standard Time", "Asia/Tbilisi"),
    ("Volgograd Standard Time", "Europe/Volgograd"),
    ("Caucasus Standard Time", "Asia/Yerevan"),
    ("Afghanistan Standard Time", "Asia/Kabul"),
    ("West Asia Standard Time", "Asia/Tashkent"),
    ("Ekaterinburg Standard Time", "Asia/Yekaterinburg"),
    ("Pakistan Standard Time", "Asia/Karachi"),
    ("Qyzylorda Standard Time", "Asia/Qyzylorda"),
    ("India Standard Time", "Asia/Kolkata"),
    ("Sri Lanka Standard Time", "Asia/Colombo"),
    ("Nepal Standard Time", "Asia/Kathmandu"),
    ("Central Asia Standard Time", "Asia/Almaty"),
    ("Bangladesh Standard Time", "Asia/Dhaka"),
    ("Omsk Standard Time", "Asia/Omsk"),
    ("Myanmar Standard Time", "Asia/Yangon"),
    ("SE Asia Standard Time", "Asia/Bangkok"),
    ("Altai Standard Time", "Asia/Barnaul"),
    ("W. Mongolia Standard Time", "Asia/Hovd"),
    ("North Asia Standard Time", "Asia/Krasnoyarsk"),
    ("N. Central Asia Standard Time", "Asia/Novosibirsk"),
    ("Tomsk Standard Time", "Asia/Tomsk"),
    ("China Standard Time", "Asia/Shanghai"),
    ("North Asia East Standard Time", "Asia/Irkutsk"),
    ("Singapore Standard Time", "Asia/Singapore"),
    ("W. Australia Standard Time", "Australia/Perth"),
    ("Taipei Standard Time", "Asia/Taipei"),
    ("Ulaanbaatar Standard Time", "Asia/Ulaanbaatar"),
    ("Aus Central W. Standard Time", "Australia/Eucla"),
    ("Transbaikal Standard Time", "Asia/Chita"),
    ("Tokyo Standard Time", "Asia/Tokyo"),
    ("North Korea Standard Time", "Asia/Pyongyang"),
    ("Korea Standard Time", "Asia/Seoul"),
    ("Yakutsk Standard Time", "Asia/Yakutsk"),
    ("Cen. Australia Standard Time", "Australia/Adelaide"),
    ("AUS Central Standard Time", "Australia/Darwin"),
    ("E. Australia Standard Time", "Australia/Brisbane"),
    ("AUS Eastern Standard Time", "Australia/Sydney"),
    ("West Pacific Standard Time", "Pacific/Port_Moresby"),
    ("Tasmania Standard Time", "Australia/Hobart"),
    ("Vladivostok Standard Time", "Asia/Vladivostok"),
    ("Lord Howe Standard Time", "Australia/Lord_Howe"),
    ("Bougainville Standard Time", "Pacific/Bougainville"),
    ("Russia Time Zone 10", "Asia/Srednekolymsk"),
    ("Magadan Standard Time", "Asia/Magadan"),
    ("Norfolk Standard Time", "Pacific/Norfolk"),
    ("Sakhalin Standard Time", "Asia/Sakhalin"),
    ("Central Pacific Standard Time", "Pacific/Guadalcanal"),
    ("Russia Time Zone 11", "Asia/Kamchatka"),
    ("New Zealand Standard Time", "Pacific/Auckland"),
    ("Fiji Standard Time", "Pacific/Fiji"),
    ("Chatham Islands Standard Time", "Pacific/Chatham"),
    ("Tonga Standard Time", "Pacific/Tongatapu"),
    ("Samoa Standard Time", "Pacific/Apia"),
    ("Line Islands Standard Time", "Pacific/Kiritimati"),
];

/// Legacy IANA names and aliases that map back to a Windows zone in [`iana_to_windows`].
const LEGACY_IANA_ALIASES: &[(&str, &str)] = &[
    ("Etc/GMT", "UTC"),
    ("Etc/UTC", "UTC"),
    ("GMT", "UTC"),
    ("Europe/Kiev", "FLE Standard Time"),
    ("Asia/Rangoon", "Myanmar Standard Time"),
    ("America/Godthab", "Greenland Standard Time"),
    ("America/Santa_Isabel", "Pacific Standard Time (Mexico)"),
    ("America/Chihuahua", "Mountain Standard Time (Mexico)"),
    ("Asia/Calcutta", "India Standard Time"),
    ("Asia/Katmandu", "Nepal Standard Time"),
    ("America/Buenos_Aires", "Argentina Standard Time"),
    ("America/Indianapolis", "US Eastern Standard Time"),
];

/// Maps a Microsoft Windows / Exchange / Outlook time zone name (with optional quotes, leading `/`
/// or `(UTC...) ` prefix) to its canonical IANA identifier.
#[must_use]
pub fn windows_to_iana(name: &str) -> Option<&'static str> {
    let clean = strip_tz_wrapper(name);
    let clean = if clean
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("(utc"))
    {
        clean.split_once(')').map_or(clean, |(_, tail)| tail.trim())
    } else {
        clean
    };
    let clean = clean.strip_prefix('/').unwrap_or(clean).trim_start();
    WINDOWS_ZONES
        .iter()
        .find(|(windows, _)| windows.eq_ignore_ascii_case(clean))
        .map(|(_, iana)| *iana)
}

/// Maps an IANA time zone name (canonical or legacy alias, with optional quotes or leading `/`) to
/// its Microsoft Windows zone name.
#[must_use]
pub fn iana_to_windows(iana: &str) -> Option<&'static str> {
    let clean = strip_tz_wrapper(iana);
    WINDOWS_ZONES
        .iter()
        .find(|(_, zone)| zone.eq_ignore_ascii_case(clean))
        .map(|(windows, _)| *windows)
        .or_else(|| {
            LEGACY_IANA_ALIASES
                .iter()
                .find(|(alias, _)| alias.eq_ignore_ascii_case(clean))
                .map(|(_, windows)| *windows)
        })
}

/// Resolves an IANA zone or Microsoft Windows / Exchange `TZID` (with optional quotes, leading `/`
/// or `(UTC...)` prefix) into a [`TimeZone`].
#[must_use]
pub fn resolve_timezone(name: &str) -> Option<TimeZone> {
    let stripped = strip_tz_wrapper(name);
    TimeZone::get(stripped)
        .ok()
        .or_else(|| windows_to_iana(stripped).and_then(|iana| TimeZone::get(iana).ok()))
}

fn strip_tz_wrapper(name: &str) -> &str {
    let trimmed = name.trim().trim_matches('"').trim();
    trimmed.strip_prefix('/').unwrap_or(trimmed).trim_start()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn unfolds_crlf_lf_spaces_tabs_and_skips_blank_lines() {
        let raw = concat!(
            "  orphan continuation\r\n",
            "BEGIN:VCALENDAR\r\n",
            "\r\n",
            "SUMMARY:Hello\r\n",
            "  world\n",
            "\t!\r\n",
            "\n",
            "END:VCALENDAR\n",
        );
        assert_eq!(
            unfold(raw),
            vec!["BEGIN:VCALENDAR", "SUMMARY:Hello world!", "END:VCALENDAR"]
        );
    }

    #[test]
    fn parses_content_lines_with_quotes_and_rfc6868() {
        let (name, params, value) = content_line(
            "organizer;cn=\"Maya; Role: Lead ^'PM^'^nNYC^^1^z^\";x-custom=ok:mailto:maya@example.com",
        )
        .expect("parses");
        assert_eq!(name, "ORGANIZER");
        assert_eq!(value, "mailto:maya@example.com");
        assert_eq!(
            params,
            BTreeMap::from([
                (
                    "CN".to_owned(),
                    "Maya; Role: Lead \"PM\"\nNYC^1^z^".to_owned()
                ),
                ("X-CUSTOM".to_owned(), "ok".to_owned()),
            ])
        );

        let (name, params, value) = content_line("SUMMARY:a:b:c").expect("parses");
        assert_eq!(name, "SUMMARY");
        assert!(params.is_empty());
        assert_eq!(value, "a:b:c");

        assert_eq!(content_line("NO_COLON"), None);
        assert_eq!(content_line(":empty_name"), None);
        assert_eq!(content_line("SUMMARY;CN=\"unclosed:value"), None);
    }

    #[test]
    fn unescapes_rfc5545_text_in_a_single_pass() {
        assert_eq!(
            unescape(r"Line 1\nLine 2\NComma\, Semi\; Path C:\\new\\notes\x\"),
            "Line 1\nLine 2\nComma, Semi; Path C:\\new\\notesx\\"
        );
    }

    #[test]
    fn strips_mailto_case_insensitively() {
        assert_eq!(strip_mailto("mailto:ada@example.com"), "ada@example.com");
        assert_eq!(
            strip_mailto("  MAILTO:ada@example.com  "),
            "ada@example.com"
        );
        assert_eq!(strip_mailto("MailTo: ada@example.com "), "ada@example.com");
        assert_eq!(strip_mailto("ada@example.com"), "ada@example.com");
        assert_eq!(strip_mailto("café"), "café");
    }

    #[test]
    fn windows_zones_has_no_duplicate_keys_and_all_iana_zones_resolve() {
        assert!(WINDOWS_ZONES.len() >= 144);
        let mut seen = HashSet::new();
        for &(windows, iana) in WINDOWS_ZONES {
            assert!(
                seen.insert(windows.to_ascii_lowercase()),
                "duplicate Windows zone key: {windows}"
            );
            assert!(
                TimeZone::get(iana).is_ok(),
                "IANA zone {iana} (for {windows}) failed to resolve in jiff"
            );
        }
        for &(alias, windows) in LEGACY_IANA_ALIASES {
            assert!(
                WINDOWS_ZONES.iter().any(|(known, _)| *known == windows),
                "legacy alias {alias} points to unknown Windows zone {windows}"
            );
            assert!(
                TimeZone::get(alias).is_ok(),
                "legacy IANA alias {alias} failed to resolve in jiff"
            );
        }
    }

    #[test]
    fn windows_iana_forward_and_reverse_mappings_handle_canonical_and_legacy_aliases() {
        assert_eq!(windows_to_iana("UTC"), Some("UTC"));
        assert_eq!(windows_to_iana("Coordinated Universal Time"), Some("UTC"));
        assert_eq!(
            windows_to_iana("(UTC) Coordinated Universal Time"),
            Some("UTC")
        );
        assert_eq!(
            windows_to_iana("(UTC-05:00) Eastern Standard Time"),
            Some("America/New_York")
        );
        assert_eq!(
            windows_to_iana("/Pacific Standard Time"),
            Some("America/Los_Angeles")
        );
        assert_eq!(
            windows_to_iana("\"(UTC+02:00) FLE Standard Time\""),
            Some("Europe/Kyiv")
        );
        assert_eq!(
            windows_to_iana("Myanmar Standard Time"),
            Some("Asia/Yangon")
        );
        assert_eq!(
            windows_to_iana("Greenland Standard Time"),
            Some("America/Nuuk")
        );
        assert_eq!(
            windows_to_iana("Pacific Standard Time (Mexico)"),
            Some("America/Tijuana")
        );
        assert_eq!(
            windows_to_iana("Mountain Standard Time (Mexico)"),
            Some("America/Mazatlan")
        );
        assert_eq!(windows_to_iana("Unknown Zone"), None);

        // Reverse lookup: modern canonical and legacy IANA aliases both map to the primary Windows name.
        for (iana, expected) in [
            ("UTC", "UTC"),
            ("Etc/GMT", "UTC"),
            ("Etc/UTC", "UTC"),
            ("America/New_York", "Eastern Standard Time"),
            ("/America/Los_Angeles", "Pacific Standard Time"),
            ("Europe/Kyiv", "FLE Standard Time"),
            ("Europe/Kiev", "FLE Standard Time"),
            ("Asia/Yangon", "Myanmar Standard Time"),
            ("Asia/Rangoon", "Myanmar Standard Time"),
            ("America/Nuuk", "Greenland Standard Time"),
            ("America/Godthab", "Greenland Standard Time"),
            ("America/Tijuana", "Pacific Standard Time (Mexico)"),
            ("America/Santa_Isabel", "Pacific Standard Time (Mexico)"),
            ("America/Mazatlan", "Mountain Standard Time (Mexico)"),
            ("America/Chihuahua", "Mountain Standard Time (Mexico)"),
        ] {
            assert_eq!(iana_to_windows(iana), Some(expected), "iana={iana}");
        }
        assert_eq!(iana_to_windows("Mars/Olympus_Mons"), None);
    }

    #[test]
    fn resolve_timezone_accepts_iana_windows_prefixes_and_leading_slashes() {
        for name in [
            "America/New_York",
            "/America/New_York",
            "\"/America/New_York\"",
            "Eastern Standard Time",
            "/Eastern Standard Time",
            "(UTC-05:00) Eastern Standard Time",
            "(UTC) Coordinated Universal Time",
            "FLE Standard Time",
            "Europe/Kiev",
        ] {
            assert!(resolve_timezone(name).is_some(), "failed to resolve {name}");
        }
        assert_eq!(
            resolve_timezone("(UTC-05:00) Eastern Standard Time")
                .and_then(|tz| tz.iana_name().map(str::to_owned)),
            Some("America/New_York".to_owned())
        );
        assert_eq!(
            resolve_timezone("FLE Standard Time").and_then(|tz| tz.iana_name().map(str::to_owned)),
            Some("Europe/Kyiv".to_owned())
        );
        assert!(resolve_timezone("Not/A_Zone").is_none());
    }
}
