//! Greeting phrase tables across supported languages, recipient name structures, and inline
//! autocomplete predictions.

use super::word_before;
use crate::editor::Editor;

/// Autocomplete waits for this many letters of a word.
const COMPLETE_AFTER: usize = 3;

/// Ghost text as it was first shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Completion {
    /// Where in the caret's block `full` starts.
    start: usize,
    /// The text from `start` the prediction completes it to.
    full: String,
    /// Whether it's a greeting.
    greeting: bool,
}

/// Someone a draft is from or to, by name: first, last, and as given.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Named {
    /// What they're called.
    pub first: String,
    /// The rest of their name after the first, or all of it if there's no more.
    pub last: String,
    /// Their name as given.
    pub full: String,
}

/// The people a draft is from and to, with `None` for one without a name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct People {
    /// From.
    pub from: Option<Named>,
    /// To, in order.
    pub to: Vec<Option<Named>>,
    /// Cc.
    pub cc: Vec<Option<Named>>,
    /// Bcc.
    pub bcc: Vec<Option<Named>>,
}

/// Each language's greetings: to first names, joined with `joiner`, and to one person's
/// last name.
struct Greetings {
    language: &'static str,
    joiner: &'static str,
    first_name: &'static [&'static str],
    last_name: &'static [&'static str],
}

const GREETINGS: &[Greetings] = &[
    Greetings {
        language: "en",
        joiner: "and",
        first_name: &[
            "Hey",
            "Hi",
            "Hello",
            "Greetings",
            "Good afternoon",
            "Good morning",
            "Good evening",
            "Dear",
            "Thank you",
            "Thanks",
            "Thank you so much",
            "Many thanks",
        ],
        last_name: &["Mr.", "Mrs.", "Mx.", "Miss", "Ms.", "Dr.", "Prof."],
    },
    Greetings {
        language: "fr",
        joiner: "et",
        first_name: &[
            "Bonjour",
            "Bonsoir",
            "Coucou",
            "Salut",
            "Merci",
            "Merci beaucoup",
            "Merci mille fois",
            "Merci bien",
            "Cimer",
        ],
        last_name: &["Cher", "Chère", "Mme."],
    },
    Greetings {
        language: "es",
        joiner: "y",
        first_name: &[
            "Hola",
            "Buenas",
            "Buenos días",
            "Buenas tardes",
            "Querido",
            "Querida",
            "Querido/a",
            "Gracias",
            "Muchas gracias",
            "Estoy muy agradecido",
            "Estoy muy agradecida",
            "Te lo agradezco",
            "Te lo agradecemos",
            "Estamos muy agradecidos",
            "Estamos muy agradecidas",
        ],
        last_name: &["Estimado/a", "Estimado", "Estimada"],
    },
    Greetings {
        language: "no",
        joiner: "og",
        first_name: &[
            "Hei",
            "Hallo",
            "God morgen",
            "God dag",
            "God kveld",
            "God ettermiddag",
            "Takk",
            "Tusen takk",
            "Mange takk",
            "Takk skal du ha",
        ],
        last_name: &["Kjære", "Herr", "Frue"],
    },
    Greetings {
        language: "sv",
        joiner: "och",
        first_name: &[
            "Hei",
            "Hallo",
            "God morgen",
            "God dag",
            "God kveld",
            "God ettermiddag",
            "Tack",
            "Tack så mycket",
            "Tackar tackar",
            "Tack ska du ha",
            "Stort tack",
            "Tack själv",
        ],
        last_name: &["Raring", "Min käre"],
    },
    Greetings {
        language: "de",
        joiner: "und",
        first_name: &[
            "Hallo",
            "Lieber",
            "Liebe",
            "Guten Morgen",
            "Guten Tag",
            "Guten Abend",
            "Danke",
            "Vielen Dank",
            "Dankeschön",
            "Ich danke Ihnen",
            "Danke sehr",
        ],
        last_name: &["Sehr geehrter", "Sehr geehrte", "Herr", "Frau"],
    },
    Greetings {
        language: "pt",
        joiner: "e",
        first_name: &[
            "Bom dia",
            "Boa tarde",
            "Boa noite",
            "Querido",
            "Querida",
            "Querido/a",
            "Caro",
            "Cara",
            "Caro/a",
            "Obrigado",
            "Obrigada",
            "Muito obrigado",
            "Muito obrigada",
        ],
        last_name: &["Prezado", "Prezada", "Prezado/a"],
    },
    Greetings {
        language: "pt-br",
        joiner: "e",
        first_name: &[
            "Bom dia",
            "Boa tarde",
            "Boa noite",
            "Querido",
            "Querida",
            "Querido/a",
            "Caro",
            "Cara",
            "Caro/a",
            "Obrigado",
            "Obrigada",
            "Muito obrigado",
            "Muito obrigada",
        ],
        last_name: &["Prezado", "Prezada", "Prezado/a"],
    },
    Greetings {
        language: "nl",
        joiner: "en",
        first_name: &[
            "Goededag",
            "Goedemorgen",
            "Goedemiddag",
            "Goedeavond",
            "Dag",
            "Beste",
            "Hallo",
            "Hoi",
            "Dankjewel",
            "Dankuwel",
            "Bedankt",
            "Heel erg bedankt",
        ],
        last_name: &["Geachte heer", "Geachte mevrouw"],
    },
    Greetings {
        language: "it",
        joiner: "e",
        first_name: &[
            "Ciao",
            "Ehi",
            "Salve",
            "Buongiorno",
            "Buona giornata",
            "Buon pomeriggio",
            "Buonasera",
            "Caro",
            "Cara",
            "Caro/a",
            "Gentile",
            "Grazie",
            "Grazie mille",
            "Molte grazie",
            "Grazie di tutto",
            "La ringrazio",
            "La ringrazio molto",
            "Ti ringrazio",
        ],
        last_name: &["Egregio", "Egregia", "Egregio/a", "Sig.", "Sig.ra"],
    },
    Greetings {
        language: "pl",
        joiner: "i",
        first_name: &[
            "Dzień dobry",
            "Dobry dzień",
            "Dobry wieczór",
            "Cześć",
            "Drogi",
            "Witam",
            "Dzięki",
            "Dziękuję",
            "Dziękuję bardzo",
            "Stokrotne dzięki",
            "Wielkie dzięki",
        ],
        last_name: &["Szanowny Panie", "Szanowna Pani", "Szanowna Pani/e"],
    },
];

/// The languages whose greetings are offered.
pub const GREETING_LANGUAGES: [&str; 11] = [
    "en", "es", "fr", "de", "it", "no", "nl", "pl", "pt", "pt-br", "sv",
];

/// Names joined as `A`, `A and B`, `A, B, and C`.
fn join_names(names: &[&str], joiner: &str) -> String {
    match names {
        [one] => (*one).to_owned(),
        [a, b] => format!("{a} {joiner} {b}"),
        [rest @ .., last] => format!("{}, {joiner} {last}", rest.join(", ")),
        [] => String::new(),
    }
}

/// Whether `text` ends with a dash and a name (`- Maya`), which nothing completes.
fn signs_off(text: &str) -> bool {
    let name = text.trim_end_matches(|ch: char| ch.is_alphabetic() || is_mark(ch));
    if name.len() == text.len() {
        return false;
    }
    let dash = name.trim_end_matches(char::is_whitespace);
    dash.strip_suffix(['-', '—'])
        .is_some_and(|before| before.chars().last().is_none_or(char::is_whitespace))
}

/// A combining mark (`\p{Mark}`), as far as names need.
fn is_mark(ch: char) -> bool {
    matches!(ch, '\u{300}'..='\u{36f}' | '\u{1ab0}'..='\u{1aff}' | '\u{1dc0}'..='\u{1dff}' | '\u{20d0}'..='\u{20ff}' | '\u{fe20}'..='\u{fe2f}')
}

/// What the editor's own predictions make of the text before the caret.
enum Local {
    /// Nothing yet, and no word completion either.
    Hold,
    /// This follows the caret, and is a greeting if `.1`.
    Predict(String, bool),
}

impl Editor {
    /// The people the draft is from and to, whose names autocomplete offers.
    pub fn set_people(&mut self, people: People) {
        self.autocorrect.people = people;
    }

    /// The languages whose greetings autocomplete offers (of [`GREETING_LANGUAGES`], and English
    /// until set).
    pub fn set_greeting_languages(&mut self, languages: Vec<String>) {
        self.autocorrect.languages = languages;
    }

    /// Ghost text at the end of the caret's line, after a character or a Backspace. While what's
    /// typed is still the start of what shows, the ghost text stays (typing its next letter takes
    /// that letter, and Backspace gives it back). Otherwise it's the editor's own prediction, a
    /// name or a greeting, or else the rest of the first of the spellchecker's completions for the
    /// word being typed.
    pub(crate) fn complete(&mut self) {
        self.ghost = None;
        let shown = self.autocorrect.completion.take();
        if !self.autocorrect.complete || !self.selection.is_collapsed() || self.trigger.is_some() {
            return;
        }
        let caret = self.selection.focus;
        let block = &self.doc.blocks[caret.block];
        // Only at the end of a paragraph.
        if !block.is_inline() || caret.offset != block.len() {
            return;
        }
        let text = block.text();
        let chars: Vec<char> = text.chars().collect();
        let before: String = chars[..caret.offset].iter().collect();
        if signs_off(&before) {
            return;
        }
        if let Some(shown) = shown.filter(|shown| shown.start < caret.offset) {
            let typed: String = chars[shown.start..caret.offset].iter().collect();
            if let Some(rest) = shown
                .full
                .strip_prefix(&typed)
                .filter(|rest| !rest.is_empty())
            {
                self.ghost = Some(rest.to_owned());
                self.autocorrect.completion = Some(shown);
                return;
            }
        }
        match self.local_prediction(caret.block == 0, &before) {
            Some(Local::Hold) => return,
            Some(Local::Predict(rest, greeting)) => {
                self.autocorrect.completion = Some(Completion {
                    start: caret.offset,
                    full: rest.clone(),
                    greeting,
                });
                self.ghost = Some(rest);
                return;
            }
            None => {}
        }
        let Some(range) = word_before(&chars, caret.offset)
            .filter(|range| range.end == caret.offset && range.len() >= COMPLETE_AFTER)
        else {
            return;
        };
        let partial: String = chars[range.clone()].iter().collect();
        let Some(engine) = self.spelling.engine.as_mut() else {
            return;
        };
        let lower = partial.to_lowercase();
        let found = engine
            .completions(&text, range.clone())
            .into_iter()
            .find(|word| {
                word.to_lowercase().starts_with(&lower)
                    && word.chars().count() > partial.chars().count()
            });
        if let Some(word) = found {
            let rest: String = word.chars().skip(partial.chars().count()).collect();
            // No punctuation at its end.
            let rest = rest
                .trim_end_matches(|ch: char| !ch.is_alphabetic())
                .to_owned();
            if !rest.is_empty() {
                self.autocorrect.completion = Some(Completion {
                    start: range.start,
                    full: format!("{partial}{rest}"),
                    greeting: false,
                });
                self.ghost = Some(rest);
            }
        }
    }

    /// The editor's own prediction for `before`, the caret's line up to the caret. It tries a
    /// name, then a greeting on the body's `first_line`.
    fn local_prediction(&self, first_line: bool, before: &str) -> Option<Local> {
        self.name_prediction(before).or_else(|| {
            first_line
                .then(|| self.greeting_prediction(before))
                .flatten()
        })
    }

    /// Completes the word being typed, when it's capitalised, as the start of a name of the
    /// draft's people, trying each person's first name, last name and whole name in turn. With one
    /// letter typed, nothing yet.
    fn name_prediction(&self, before: &str) -> Option<Local> {
        if self.autocorrect.greeted {
            return None;
        }
        // The last word, with any punctuation it has.
        let word = before
            .rsplit(char::is_whitespace)
            .next()
            .unwrap_or_default();
        let token = word.trim_matches(|ch: char| !ch.is_alphanumeric());
        if token
            .chars()
            .next()
            .is_none_or(|first| first.is_lowercase())
        {
            return None;
        }
        let people = &self.autocorrect.people;
        let named = people.from.iter().chain(
            people
                .to
                .iter()
                .chain(&people.cc)
                .chain(&people.bcc)
                .flatten(),
        );
        let mut names = named
            .flat_map(|person| [&person.first, &person.last, &person.full])
            .map(|name| name.trim())
            .filter(|name| name.starts_with(word) && *name != word);
        let first = names.next()?;
        Some(if token.chars().count() < 2 {
            Local::Hold
        } else {
            Local::Predict(first[word.len()..].to_owned(), false)
        })
    }

    /// A greeting to the one or two people the draft is to, all named, by first name (with one
    /// person, also with those the draft copies), or to one person by last name. It's offered once
    /// the greeting's first word is typed, and nothing is offered until then.
    fn greeting_prediction(&self, before: &str) -> Option<Local> {
        let people = &self.autocorrect.people;
        let to: Vec<&Named> = people
            .to
            .iter()
            .map(Option::as_ref)
            .collect::<Option<_>>()?;
        let cc_named = people.cc.iter().all(Option::is_some);
        let firsts: Vec<&str> = to.iter().map(|person| person.first.as_str()).collect();
        let with_cc: Vec<&str> = firsts
            .iter()
            .copied()
            .chain(
                people
                    .cc
                    .iter()
                    .flatten()
                    .map(|person| person.first.as_str()),
            )
            .collect();
        let english = ["en".to_owned()];
        let languages = if self.autocorrect.languages.is_empty() {
            &english[..]
        } else {
            &self.autocorrect.languages[..]
        };
        let mut candidates = Vec::new();
        for greetings in GREETINGS.iter().filter(|greetings| {
            languages
                .iter()
                .any(|language| language == greetings.language)
        }) {
            for greeting in greetings.first_name {
                if matches!(to.len(), 1 | 2) {
                    candidates.push(format!(
                        "{greeting} {}",
                        join_names(&firsts, greetings.joiner)
                    ));
                }
                if to.len() == 1 && cc_named && !people.cc.is_empty() {
                    candidates.push(format!(
                        "{greeting} {}",
                        join_names(&with_cc, greetings.joiner)
                    ));
                }
            }
            if let [one] = to[..] {
                candidates.extend(
                    greetings
                        .last_name
                        .iter()
                        .map(|greeting| format!("{greeting} {}", one.last)),
                );
            }
        }
        candidates.retain(|candidate| candidate.starts_with(before) && candidate != before);
        // Its first word typed whole.
        let typed = candidates.iter().find(|candidate| {
            before.starts_with(
                candidate
                    .split(|ch: char| !ch.is_alphabetic() && !is_mark(ch))
                    .next()
                    .unwrap_or_default(),
            )
        });
        match typed {
            Some(greeting) => Some(Local::Predict(greeting[before.len()..].to_owned(), true)),
            None => (!candidates.is_empty()).then_some(Local::Hold),
        }
    }

    /// Ghost text was taken. A taken greeting keeps names from being offered until something
    /// other than a space or Backspace is typed.
    pub(crate) fn took_completion(&mut self) {
        self.autocorrect.greeted = self
            .autocorrect
            .completion
            .take()
            .is_some_and(|completion| completion.greeting);
    }
}
