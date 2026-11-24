//! The time zones a time can be typed in (`3pm pst`, `9am london`, `at 2 in new york`): their
//! tables, and how the parser reads them. The tables' order is the order a prefix lists names in.

use std::{
    collections::{HashMap, HashSet},
    sync::OnceLock,
};

use jiff::tz::TimeZone;

/// What a time zone token stands for.
pub(super) struct Zone {
    /// The zone, `None` for a name the tables can't resolve (`eastern time`, whose abbreviation
    /// is mixed-case), which reads as the local zone.
    pub tz: Result<Option<TimeZone>, InvalidZone>,
    /// How the label names it: ` PST`, ` in London`, ` in New York`.
    pub label: String,
}

/// A token names a zone that doesn't exist (`gmt+1`, kept as typed), so the whole parse comes
/// back empty.
#[derive(Debug)]
pub(super) struct InvalidZone;

struct Tables {
    /// Every name, lower-case with `_` for spaces: abbreviations, then places.
    names: Vec<String>,
    /// The names that aren't abbreviations.
    places: HashSet<String>,
    /// The few names that cost less confidence.
    well_known: HashSet<String>,
    /// Names of more than one word, by their first.
    by_first_word: HashMap<String, Vec<String>>,
    /// Zones by their last segment, lower-case.
    by_segment: HashMap<String, &'static str>,
}

fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(|| {
        let abbreviations: Vec<String> = ABBREVIATIONS
            .iter()
            .map(|(name, _)| name.to_lowercase())
            .collect();
        let mut seen_segments = HashSet::new();
        let mut by_segment: Vec<(String, &'static str)> = Vec::new();
        // Prefer three-segment canonical zones (`America/Argentina/Buenos_Aires`) when a two-segment
        // alias (`America/Buenos_Aires`) shares the same leaf name.
        let mut ordered: Vec<&'static str> = zones().iter().map(String::as_str).collect();
        ordered.sort_by(|a, b| {
            b.matches('/')
                .count()
                .cmp(&a.matches('/').count())
                .then_with(|| a.starts_with("Etc/").cmp(&b.starts_with("Etc/")))
                .then_with(|| a.cmp(b))
        });
        for zone in ordered {
            let seg = segment(zone);
            if seen_segments.insert(seg.clone()) {
                by_segment.push((seg, zone));
            }
        }
        by_segment.sort_by(|(_, a), (_, b)| {
            a.starts_with("Etc/")
                .cmp(&b.starts_with("Etc/"))
                .then_with(|| a.cmp(b))
        });
        let places: Vec<String> = PLACE_ABBREVIATIONS
            .iter()
            .map(|(name, _)| name.to_lowercase())
            .chain(
                PLACES
                    .iter()
                    .map(|(name, _)| name.replace(' ', "_").to_lowercase()),
            )
            .chain(by_segment.iter().map(|(segment, _)| segment.clone()))
            .collect();
        let well_known = ["new_york", "san_francisco", "paris"]
            .into_iter()
            .map(str::to_owned)
            .chain(abbreviations.iter().cloned())
            .chain(
                PLACE_ABBREVIATIONS
                    .iter()
                    .map(|(name, _)| name.to_lowercase()),
            )
            .collect();
        let names: Vec<String> = abbreviations
            .into_iter()
            .chain(places.iter().cloned())
            .collect();
        let mut by_first_word: HashMap<String, Vec<String>> = HashMap::new();
        for name in names.iter().filter(|name| name.contains('_')) {
            let first = name.split('_').next().unwrap_or_default();
            by_first_word
                .entry(first.to_owned())
                .or_default()
                .push(name.clone());
        }
        Tables {
            names,
            places: places.into_iter().collect(),
            well_known,
            by_first_word,
            by_segment: by_segment.into_iter().collect(),
        }
    })
}

/// A zone's last segment as a name: `Europe/London` is `london`, and `America/Port-au-Prince` is
/// `port_au-prince` (only the first `-` becomes `_`).
fn segment(zone: &str) -> String {
    zone.rsplit('/')
        .next()
        .unwrap_or(zone)
        .replacen('-', "_", 1)
        .to_lowercase()
}

/// Every zone name, in order.
pub(super) fn names() -> &'static [String] {
    &tables().names
}

/// The multi-word names starting with `word` (`new` → `new_york`, `new_jersey`, …).
pub(super) fn starting_with(word: &str) -> Option<&'static [String]> {
    tables().by_first_word.get(word).map(Vec::as_slice)
}

/// A place's name, not an abbreviation, so `time` after it is dropped (`london time`).
pub(super) fn is_place(name: &str) -> bool {
    tables().places.contains(name)
}

/// A zone typed often enough to cost less confidence than the rest.
pub(super) fn is_well_known(name: &str) -> bool {
    tables().well_known.contains(name)
}

/// What `token` stands for: an abbreviation, a zone's last segment, a place's short name, or a
/// place.
pub(super) fn resolve(token: &str) -> Zone {
    let upper = token.to_uppercase();
    let title = || format!("in {}", title_case(&token.replace('_', " ")));
    let (zone, label) = if let Some((_, zone)) =
        ABBREVIATIONS.iter().find(|(name, _)| *name == upper)
    {
        (Some(*zone), upper)
    } else if upper.contains("GMT+") {
        return Zone {
            tz: Err(InvalidZone),
            label: upper,
        };
    } else if let Some(zone) = tables().by_segment.get(token) {
        (Some(*zone), title())
    } else if let Some((_, place)) = PLACE_ABBREVIATIONS.iter().find(|(name, _)| *name == upper) {
        (
            PLACES
                .iter()
                .find(|(name, _)| name == place)
                .map(|(_, zone)| *zone),
            format!("in {place}"),
        )
    } else {
        let name = token.replace('_', " ").to_lowercase();
        (
            PLACES
                .iter()
                .find(|(place, _)| place.to_lowercase() == name)
                .map(|(_, zone)| *zone),
            title(),
        )
    };
    Zone {
        tz: zone.map(TimeZone::get).transpose().map_err(|_| InvalidZone),
        label,
    }
}

/// Title case: each run starting with a word character (`\w\S*`) gets an upper-case first
/// letter and the rest lower-cased, so `åland islands` reads `åLand Islands`.
fn title_case(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_run = false;
    for ch in text.chars() {
        if in_run {
            in_run = !ch.is_whitespace();
            out.extend(ch.to_lowercase());
        } else if ch.is_ascii_alphanumeric() || ch == '_' {
            in_run = true;
            out.extend(ch.to_uppercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// A table of names and what they stand for.
type Table = &'static [(&'static str, &'static str)];

/// The tables as other searches read them: places, abbreviations, places' short names and IANA
/// zones.
pub(crate) fn tables_for_search() -> (Table, Table, Table, &'static [String]) {
    (PLACES, ABBREVIATIONS, PLACE_ABBREVIATIONS, zones())
}

/// Abbreviations, matched upper-cased and shown that way.
const ABBREVIATIONS: &[(&str, &str)] = &[
    ("CEST", "Europe/Berlin"),
    ("CET", "Europe/Berlin"),
    ("AST", "America/Halifax"),
    ("WAT", "Africa/Lagos"),
    ("NZST", "Pacific/Auckland"),
    ("NZDT", "Pacific/Auckland"),
    ("SST", "Pacific/Pago_Pago"),
    ("AEST", "Australia/Hobart"),
    ("AEDT", "Australia/Hobart"),
    ("ACST", "Australia/Broken_Hill"),
    ("ACDT", "Australia/Broken_Hill"),
    ("AWST", "Australia/Perth"),
    ("EEST", "Europe/Kyiv"),
    ("EET", "Europe/Kyiv"),
    ("GMT", "Etc/GMT"),
    ("CAT", "Africa/Maputo"),
    ("ADT", "America/Halifax"),
    ("EDT", "America/New_York"),
    ("EST", "America/New_York"),
    ("CST", "America/Chicago"),
    ("NDT", "America/St_Johns"),
    ("NST", "America/St_Johns"),
    ("CDT", "America/Chicago"),
    ("MDT", "America/Denver"),
    ("MST", "America/Denver"),
    ("PDT", "America/Los_Angeles"),
    ("PST", "America/Los_Angeles"),
    ("EAT", "Africa/Nairobi"),
    ("WEST", "Atlantic/Canary"),
    ("WET", "Atlantic/Canary"),
    ("BST", "Europe/London"),
    ("HKT", "Asia/Hong_Kong"),
    ("WIB", "Asia/Jakarta"),
    ("WITA", "Asia/Makassar"),
    ("WIT", "Asia/Jayapura"),
    ("IST", "Asia/Kolkata"),
    ("IDT", "Asia/Jerusalem"),
    ("JST", "Asia/Tokyo"),
    ("KST", "Asia/Seoul"),
    ("SAST", "Africa/Johannesburg"),
    ("PKT", "Asia/Karachi"),
    ("MSK", "Europe/Moscow"),
    ("HST", "Pacific/Honolulu"),
    ("AKDT", "America/Anchorage"),
    ("AKST", "America/Anchorage"),
    ("HDT", "America/Adak"),
    ("ET", "America/New_York"),
    ("Eastern Time", "America/New_York"),
    ("PT", "America/Los_Angeles"),
    ("Pacific Time", "America/Los_Angeles"),
    ("CT", "America/Chicago"),
    ("Central Time", "America/Chicago"),
    ("MT", "America/Denver"),
    ("Mountain Time", "America/Denver"),
    ("SGT", "Asia/Singapore"),
    ("UTC", "Etc/UTC"),
];

/// Places' short names, shown as the place they stand for.
const PLACE_ABBREVIATIONS: &[(&str, &str)] = &[
    ("NY", "New York"),
    ("NYC", "New York"),
    ("UK", "United Kingdom"),
    ("LA", "Los Angeles"),
    ("LAX", "Los Angeles"),
    ("SF", "San Francisco"),
    ("SLC", "Salt Lake City"),
];

/// Places.
const PLACES: &[(&str, &str)] = &[
    ("Alabama", "America/Chicago"),
    ("Alaska", "America/Anchorage"),
    ("Albuquerque", "America/Denver"),
    ("Arizona", "America/Phoenix"),
    ("Arkansas", "America/Chicago"),
    ("Atlanta", "America/New_York"),
    ("Austin", "America/Chicago"),
    ("Baltimore", "America/New_York"),
    ("Brooklyn", "America/New_York"),
    ("Boston", "America/New_York"),
    ("California", "America/Los_Angeles"),
    ("Charlotte", "America/New_York"),
    ("Cleveland", "America/New_York"),
    ("Colorado", "America/Denver"),
    ("Columbus", "America/New_York"),
    ("Connecticut", "America/New_York"),
    ("Dallas", "America/Chicago"),
    ("Delaware", "America/New_York"),
    ("El Paso", "America/Denver"),
    ("Florida", "America/New_York"),
    ("Fort Worth", "America/Chicago"),
    ("Fresno", "America/Los_Angeles"),
    ("Georgia", "America/New_York"),
    ("Hawaii", "Pacific/Honolulu"),
    ("Houston", "America/Chicago"),
    ("Idaho", "America/Denver"),
    ("Illinois", "America/Chicago"),
    ("Indiana", "America/Indiana/Indianapolis"),
    ("Iowa", "America/Chicago"),
    ("Jacksonville", "America/New_York"),
    ("Kansas", "America/Chicago"),
    ("Kentucky", "America/New_York"),
    ("Kansas City, MO", "America/Chicago"),
    ("Las Vegas", "America/Los_Angeles"),
    ("Louisiana", "America/Chicago"),
    ("Los Angeles", "America/Los_Angeles"),
    ("Long Beach", "America/Los_Angeles"),
    ("Maine", "America/New_York"),
    ("Maryland", "America/New_York"),
    ("Massachusetts", "America/New_York"),
    ("Miami", "America/New_York"),
    ("Michigan", "America/New_York"),
    ("Milwaukee", "America/Chicago"),
    ("Minneapolis", "America/Chicago"),
    ("Minnesota", "America/Chicago"),
    ("Mississippi", "America/Chicago"),
    ("Missouri", "America/Chicago"),
    ("Memphis", "America/Chicago"),
    ("Mesa", "America/Phoenix"),
    ("Montana", "America/Denver"),
    ("Montreal", "America/Toronto"),
    ("Nashville", "America/Chicago"),
    ("Nebraska", "America/Chicago"),
    ("Nevada", "America/Los_Angeles"),
    ("New Hampshire", "America/New_York"),
    ("New Jersey", "America/New_York"),
    ("New Mexico", "America/Denver"),
    ("New Orleans", "America/Chicago"),
    ("New York", "America/New_York"),
    ("North Carolina", "America/New_York"),
    ("North Dakota", "America/Chicago"),
    ("Oakland", "America/Los_Angeles"),
    ("Ohio", "America/New_York"),
    ("Oklahoma", "America/Chicago"),
    ("Omaha", "America/Chicago"),
    ("Oregon", "America/Los_Angeles"),
    ("Ottawa", "America/Toronto"),
    ("Pennsylvania", "America/New_York"),
    ("Philadelphia", "America/New_York"),
    ("Raleigh", "America/New_York"),
    ("Portland, OR", "America/Los_Angeles"),
    ("Rhode Island", "America/New_York"),
    ("San Francisco", "America/Los_Angeles"),
    ("San Diego", "America/Los_Angeles"),
    ("Sacramento", "America/Los_Angeles"),
    ("Salt Lake City", "America/Denver"),
    ("San Antonio", "America/Chicago"),
    ("San Jose", "America/Los_Angeles"),
    ("Seattle", "America/Los_Angeles"),
    ("South Carolina", "America/New_York"),
    ("South Dakota", "America/Chicago"),
    ("Tennessee", "America/Chicago"),
    ("Texas", "America/Chicago"),
    ("Tucson", "America/Phoenix"),
    ("Tulsa", "America/Chicago"),
    ("Utah", "America/Denver"),
    ("Vermont", "America/New_York"),
    ("Virginia", "America/New_York"),
    ("Washington", "America/Los_Angeles"),
    ("Washington DC", "America/New_York"),
    ("West Virginia", "America/New_York"),
    ("Wichita", "America/Chicago"),
    ("Wisconsin", "America/Chicago"),
    ("Wyoming", "America/Denver"),
    ("Calgary", "America/Edmonton"),
    ("Barcelona", "Europe/Madrid"),
    ("Shanghai", "Asia/Shanghai"),
    ("Beijing", "Asia/Shanghai"),
    ("Karachi", "Asia/Karachi"),
    ("Shenzhen", "Asia/Shanghai"),
    ("Guangzhou", "Asia/Shanghai"),
    ("Mumbai", "Asia/Kolkata"),
    ("Moscow", "Europe/Moscow"),
    ("Kinshasa", "Africa/Kinshasa"),
    ("Tianjin", "Asia/Shanghai"),
    ("Lahore", "Asia/Karachi"),
    ("Delhi", "Asia/Kolkata"),
    ("Jakarta", "Asia/Jakarta"),
    ("Dongguan", "Asia/Shanghai"),
    ("Lagos", "Africa/Lagos"),
    ("Seoul", "Asia/Seoul"),
    ("Foshan", "Asia/Shanghai"),
    ("Tokyo", "Asia/Tokyo"),
    ("Chengdu", "Asia/Shanghai"),
    ("Mexico City", "America/Mexico_City"),
    ("London", "Europe/London"),
    ("Birmingham", "Europe/London"),
    ("Manchester", "Europe/London"),
    ("United Kingdom", "Europe/London"),
    ("Dhaka", "Asia/Dhaka"),
    ("Wuhan", "Asia/Shanghai"),
    ("Cairo", "Africa/Cairo"),
    ("Ningbo", "Asia/Shanghai"),
    ("Hyderabad", "Asia/Kolkata"),
    ("Hong Kong", "Asia/Hong_Kong"),
    ("Nanjing", "Asia/Shanghai"),
    ("Chongqing", "Asia/Shanghai"),
    ("Changsha", "Asia/Shanghai"),
    ("Hangzhou", "Asia/Shanghai"),
    ("Ahmedabad", "Asia/Kolkata"),
    ("Baghdad", "Asia/Baghdad"),
    ("Chennai", "Asia/Kolkata"),
    ("Riyadh", "Asia/Riyadh"),
    ("Rio de Janeiro", "America/Sao_Paulo"),
    ("Shenyang", "Asia/Shanghai"),
    ("Xi'an", "Asia/Shanghai"),
    ("Suzhou", "Asia/Shanghai"),
    ("Surat", "Asia/Kolkata"),
    ("Shantou", "Asia/Shanghai"),
    ("Harbin", "Asia/Shanghai"),
    ("Dar es Salaam", "Africa/Dar_es_Salaam"),
    ("Saint Petersburg", "Europe/Moscow"),
    ("Tel Aviv", "Asia/Jerusalem"),
    ("Jerusalem", "Asia/Jerusalem"),
    ("Kolkata", "Asia/Kolkata"),
    ("Calcutta", "Asia/Kolkata"),
    ("Bangalore", "Asia/Kolkata"),
    ("Andorra", "Europe/Andorra"),
    ("United Arab Emirates", "Asia/Dubai"),
    ("Afghanistan", "Asia/Kabul"),
    ("Antigua & Barbuda", "America/Port_of_Spain"),
    ("Anguilla", "America/Port_of_Spain"),
    ("Albania", "Europe/Tirane"),
    ("Armenia", "Asia/Yerevan"),
    ("Angola", "Africa/Lagos"),
    ("Antarctica", "Antarctica/Casey"),
    ("Argentina", "America/Argentina/Buenos_Aires"),
    ("Samoa (American)", "Pacific/Pago_Pago"),
    ("Austria", "Europe/Vienna"),
    ("Australia", "Australia/Sydney"),
    ("Aruba", "America/Curacao"),
    ("Åland Islands", "Europe/Helsinki"),
    ("Azerbaijan", "Asia/Baku"),
    ("Bosnia & Herzegovina", "Europe/Belgrade"),
    ("Barbados", "America/Barbados"),
    ("Bangladesh", "Asia/Dhaka"),
    ("Belgium", "Europe/Brussels"),
    ("Burkina Faso", "Africa/Abidjan"),
    ("Bulgaria", "Europe/Sofia"),
    ("Bahrain", "Asia/Qatar"),
    ("Burundi", "Africa/Maputo"),
    ("Benin", "Africa/Lagos"),
    ("St. Barthelemy", "America/Port_of_Spain"),
    ("Bermuda", "Atlantic/Bermuda"),
    ("Brunei", "Asia/Brunei"),
    ("Bolivia", "America/La_Paz"),
    ("Caribbean NL", "America/Curacao"),
    ("Brazil", "America/Sao_Paulo"),
    ("Bahamas", "America/Nassau"),
    ("Bhutan", "Asia/Thimphu"),
    ("Botswana", "Africa/Maputo"),
    ("Belarus", "Europe/Minsk"),
    ("Belize", "America/Belize"),
    ("Canada", "America/Toronto"),
    ("Cocos (Keeling) Islands", "Indian/Cocos"),
    ("Congo (Dem. Rep.)", "Africa/Maputo"),
    ("Central African Rep.", "Africa/Lagos"),
    ("Congo (Rep.)", "Africa/Lagos"),
    ("Switzerland", "Europe/Zurich"),
    ("Côte d'Ivoire", "Africa/Abidjan"),
    ("Cook Islands", "Pacific/Rarotonga"),
    ("Chile", "America/Santiago"),
    ("Cameroon", "Africa/Lagos"),
    ("China", "Asia/Shanghai"),
    ("Colombia", "America/Bogota"),
    ("Costa Rica", "America/Costa_Rica"),
    ("Cuba", "America/Havana"),
    ("Cape Verde", "Atlantic/Cape_Verde"),
    ("Curaçao", "America/Curacao"),
    ("Christmas Island", "Indian/Christmas"),
    ("Cyprus", "Asia/Nicosia"),
    ("Czech Republic", "Europe/Prague"),
    ("Germany", "Europe/Berlin"),
    ("Djibouti", "Africa/Nairobi"),
    ("Denmark", "Europe/Copenhagen"),
    ("Dominica", "America/Port_of_Spain"),
    ("Dominican Republic", "America/Santo_Domingo"),
    ("Algeria", "Africa/Algiers"),
    ("Ecuador", "America/Guayaquil"),
    ("Estonia", "Europe/Tallinn"),
    ("Egypt", "Africa/Cairo"),
    ("Western Sahara", "Africa/El_Aaiun"),
    ("Eritrea", "Africa/Nairobi"),
    ("Spain", "Europe/Madrid"),
    ("Ethiopia", "Africa/Nairobi"),
    ("Finland", "Europe/Helsinki"),
    ("Fiji", "Pacific/Fiji"),
    ("Falkland Islands", "Atlantic/Stanley"),
    ("Micronesia", "Pacific/Chuuk"),
    ("Faroe Islands", "Atlantic/Faroe"),
    ("France", "Europe/Paris"),
    ("Gabon", "Africa/Lagos"),
    ("Britain (UK)", "Europe/London"),
    ("England", "Europe/London"),
    ("Grenada", "America/Port_of_Spain"),
    ("French Guiana", "America/Cayenne"),
    ("Guernsey", "Europe/London"),
    ("Ghana", "Africa/Accra"),
    ("Gibraltar", "Europe/Gibraltar"),
    ("Greenland", "America/Godthab"),
    ("Gambia", "Africa/Abidjan"),
    ("Guinea", "Africa/Abidjan"),
    ("Guadeloupe", "America/Port_of_Spain"),
    ("Equatorial Guinea", "Africa/Lagos"),
    ("Greece", "Europe/Athens"),
    (
        "South Georgia & the South Sandwich Islands",
        "Atlantic/South_Georgia",
    ),
    ("Guatemala", "America/Guatemala"),
    ("Guam", "Pacific/Guam"),
    ("Guinea-Bissau", "Africa/Bissau"),
    ("Guyana", "America/Guyana"),
    ("Honduras", "America/Tegucigalpa"),
    ("Croatia", "Europe/Belgrade"),
    ("Haiti", "America/Port-au-Prince"),
    ("Hungary", "Europe/Budapest"),
    ("Indonesia", "Asia/Jakarta"),
    ("Ireland", "Europe/Dublin"),
    ("Belfast", "Europe/London"),
    ("Israel", "Asia/Jerusalem"),
    ("Isle of Man", "Europe/London"),
    ("India", "Asia/Kolkata"),
    ("British Indian Ocean Territory", "Indian/Chagos"),
    ("Iraq", "Asia/Baghdad"),
    ("Iran", "Asia/Tehran"),
    ("Iceland", "Atlantic/Reykjavik"),
    ("Italy", "Europe/Rome"),
    ("Jersey", "Europe/London"),
    ("Jamaica", "America/Jamaica"),
    ("Jordan", "Asia/Amman"),
    ("Japan", "Asia/Tokyo"),
    ("Kenya", "Africa/Nairobi"),
    ("Kyrgyzstan", "Asia/Bishkek"),
    ("Cambodia", "Asia/Bangkok"),
    ("Kiribati", "Pacific/Tarawa"),
    ("Comoros", "Africa/Nairobi"),
    ("St. Kitts & Nevis", "America/Port_of_Spain"),
    ("Korea (South)", "Asia/Seoul"),
    ("Korea (North)", "Asia/Pyongyang"),
    ("Kuwait", "Asia/Riyadh"),
    ("Cayman Islands", "America/Panama"),
    ("Kazakhstan", "Asia/Almaty"),
    ("Laos", "Asia/Bangkok"),
    ("Lebanon", "Asia/Beirut"),
    ("St. Lucia", "America/Port_of_Spain"),
    ("Liechtenstein", "Europe/Zurich"),
    ("Sri Lanka", "Asia/Colombo"),
    ("Liberia", "Africa/Monrovia"),
    ("Lesotho", "Africa/Johannesburg"),
    ("Lithuania", "Europe/Vilnius"),
    ("Luxembourg", "Europe/Luxembourg"),
    ("Latvia", "Europe/Riga"),
    ("Libya", "Africa/Tripoli"),
    ("Morocco", "Africa/Casablanca"),
    ("Monaco", "Europe/Monaco"),
    ("Moldova", "Europe/Chisinau"),
    ("Montenegro", "Europe/Belgrade"),
    ("St. Martin (French)", "America/Port_of_Spain"),
    ("Madagascar", "Africa/Nairobi"),
    ("Marshall Islands", "Pacific/Majuro"),
    ("Macedonia", "Europe/Belgrade"),
    ("Mali", "Africa/Abidjan"),
    ("Myanmar (Burma)", "Asia/Yangon"),
    ("Mongolia", "Asia/Ulaanbaatar"),
    ("Macau", "Asia/Macau"),
    ("Northern Mariana Islands", "Pacific/Guam"),
    ("Martinique", "America/Martinique"),
    ("Mauritania", "Africa/Abidjan"),
    ("Montserrat", "America/Port_of_Spain"),
    ("Malta", "Europe/Malta"),
    ("Mauritius", "Indian/Mauritius"),
    ("Maldives", "Indian/Maldives"),
    ("Malawi", "Africa/Maputo"),
    ("Mexico", "America/Mexico_City"),
    ("Malaysia", "Asia/Kuala_Lumpur"),
    ("Mozambique", "Africa/Maputo"),
    ("Namibia", "Africa/Windhoek"),
    ("New Caledonia", "Pacific/Noumea"),
    ("Niger", "Africa/Lagos"),
    ("Norfolk Island", "Pacific/Norfolk"),
    ("Nigeria", "Africa/Lagos"),
    ("Nicaragua", "America/Managua"),
    ("Netherlands", "Europe/Amsterdam"),
    ("Norway", "Europe/Oslo"),
    ("Nepal", "Asia/Kathmandu"),
    ("Nauru", "Pacific/Nauru"),
    ("Niue", "Pacific/Niue"),
    ("New Zealand", "Pacific/Auckland"),
    ("Oman", "Asia/Dubai"),
    ("Panama", "America/Panama"),
    ("Peru", "America/Lima"),
    ("French Polynesia", "Pacific/Tahiti"),
    ("Papua New Guinea", "Pacific/Port_Moresby"),
    ("Philippines", "Asia/Manila"),
    ("Pakistan", "Asia/Karachi"),
    ("Poland", "Europe/Warsaw"),
    ("St. Pierre & Miquelon", "America/Miquelon"),
    ("Pitcairn", "Pacific/Pitcairn"),
    ("Puerto Rico", "America/Puerto_Rico"),
    ("Palestine", "Asia/Gaza"),
    ("Portugal", "Europe/Lisbon"),
    ("Palau", "Pacific/Palau"),
    ("Paraguay", "America/Asuncion"),
    ("Qatar", "Asia/Qatar"),
    ("Réunion", "Indian/Reunion"),
    ("Romania", "Europe/Bucharest"),
    ("Serbia", "Europe/Belgrade"),
    ("Russia", "Europe/Moscow"),
    ("Rwanda", "Africa/Maputo"),
    ("Saudi Arabia", "Asia/Riyadh"),
    ("Solomon Islands", "Pacific/Guadalcanal"),
    ("Seychelles", "Indian/Mahe"),
    ("Sudan", "Africa/Khartoum"),
    ("Sweden", "Europe/Stockholm"),
    ("Singapore", "Asia/Singapore"),
    ("St. Helena", "Africa/Abidjan"),
    ("Slovenia", "Europe/Belgrade"),
    ("Svalbard & Jan Mayen", "Europe/Oslo"),
    ("Slovakia", "Europe/Prague"),
    ("Sierra Leone", "Africa/Abidjan"),
    ("San Marino", "Europe/Rome"),
    ("Senegal", "Africa/Abidjan"),
    ("Somalia", "Africa/Nairobi"),
    ("Suriname", "America/Paramaribo"),
    ("South Sudan", "Africa/Juba"),
    ("Sao Tome & Principe", "Africa/Sao_Tome"),
    ("El Salvador", "America/El_Salvador"),
    ("St. Maarten (Dutch)", "America/Curacao"),
    ("Syria", "Asia/Damascus"),
    ("Swaziland", "Africa/Johannesburg"),
    ("Turks & Caicos Islands", "America/Grand_Turk"),
    ("Chad", "Africa/Ndjamena"),
    ("French Southern & Antarctic Lands", "Indian/Kerguelen"),
    ("Togo", "Africa/Abidjan"),
    ("Thailand", "Asia/Bangkok"),
    ("Tajikistan", "Asia/Dushanbe"),
    ("Tokelau", "Pacific/Fakaofo"),
    ("East Timor", "Asia/Dili"),
    ("Turkmenistan", "Asia/Ashgabat"),
    ("Tunisia", "Africa/Tunis"),
    ("Tonga", "Pacific/Tongatapu"),
    ("Turkey", "Europe/Istanbul"),
    ("Trinidad & Tobago", "America/Port_of_Spain"),
    ("Tuvalu", "Pacific/Funafuti"),
    ("Taiwan", "Asia/Taipei"),
    ("Tanzania", "Africa/Nairobi"),
    ("Ukraine", "Europe/Kyiv"),
    ("Uganda", "Africa/Nairobi"),
    ("US minor outlying islands", "Pacific/Wake"),
    ("United States", "America/New_York"),
    ("Uruguay", "America/Montevideo"),
    ("Uzbekistan", "Asia/Samarkand"),
    ("Vatican City", "Europe/Rome"),
    ("St. Vincent", "America/Port_of_Spain"),
    ("Venezuela", "America/Caracas"),
    ("Virgin Islands (UK)", "America/Port_of_Spain"),
    ("Virgin Islands (US)", "America/Port_of_Spain"),
    ("Vietnam", "Asia/Ho_Chi_Minh"),
    ("Vanuatu", "Pacific/Efate"),
    ("Wallis & Futuna", "Pacific/Wallis"),
    ("Samoa (Western)", "Pacific/Apia"),
    ("Yemen", "Asia/Riyadh"),
    ("Mayotte", "Africa/Nairobi"),
    ("South Africa", "Africa/Johannesburg"),
    ("Zambia", "Africa/Maputo"),
    ("Zimbabwe", "Africa/Maputo"),
    ("Alberta", "America/Edmonton"),
    ("British Columbia", "America/Vancouver"),
    ("Manitoba", "America/Winnipeg"),
    ("New Brunswick", "America/Halifax"),
    ("Newfoundland and Labrador", "America/St_Johns"),
    ("Nova Scotia", "America/Halifax"),
    ("Ontario", "America/Toronto"),
    ("Prince Edward Island", "America/Halifax"),
    ("Quebec", "America/Toronto"),
    ("Saskatchewan", "America/Regina"),
];

/// IANA zones from the system database, each also known by its last segment (`london`).
fn zones() -> &'static [String] {
    static ZONES: OnceLock<Vec<String>> = OnceLock::new();
    ZONES.get_or_init(|| {
        let mut list: Vec<String> = jiff::tz::db()
            .available()
            .map(|name| name.to_string())
            .filter(|zone| is_iana_zone(zone))
            .collect();
        list.sort_by(|a, b| {
            a.starts_with("Etc/")
                .cmp(&b.starts_with("Etc/"))
                .then_with(|| a.cmp(b))
        });
        list.dedup();
        list
    })
}

fn is_iana_zone(zone: &str) -> bool {
    const AREAS: &[&str] = &[
        "Africa/",
        "America/",
        "Antarctica/",
        "Arctic/",
        "Asia/",
        "Atlantic/",
        "Australia/",
        "Europe/",
        "Indian/",
        "Pacific/",
    ];
    if AREAS.iter().any(|area| zone.starts_with(area)) {
        return true;
    }
    match zone {
        "Etc/UTC" | "Etc/GMT" => true,
        _ => zone
            .strip_prefix("Etc/GMT+")
            .or_else(|| zone.strip_prefix("Etc/GMT-"))
            .is_some_and(|offset| offset != "0" && offset.bytes().all(|b| b.is_ascii_digit())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_resolve_to_zones_and_labels() {
        let label = |token: &str| resolve(token).label;
        assert_eq!(label("pst"), "PST");
        assert_eq!(label("london"), "in London");
        assert_eq!(label("new_york"), "in New York");
        assert_eq!(label("nyc"), "in New York");
        assert_eq!(label("kansas_city,_mo"), "in Kansas City, Mo");
        assert_eq!(label("åland_islands"), "in åLand Islands");
        assert!(resolve("gmt+1").tz.is_err());
        assert!(resolve("eastern time").tz.is_ok_and(|tz| tz.is_none()));
        assert_eq!(starting_with("new").map(<[String]>::len), Some(10));
        assert!(is_place("london") && !is_place("pst") && is_well_known("paris"));
        for (token, iana) in [
            ("nuuk", "America/Nuuk"),
            ("ciudad_juarez", "America/Ciudad_Juarez"),
            ("kanton", "Pacific/Kanton"),
        ] {
            assert_eq!(
                resolve(token)
                    .tz
                    .ok()
                    .flatten()
                    .and_then(|tz| tz.iana_name().map(str::to_owned)),
                Some(iana.to_owned())
            );
        }
    }
}
