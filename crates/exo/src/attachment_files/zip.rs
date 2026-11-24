//! The zip for `Download all`, which holds a message's files in one deflated archive named for the
//! conversation. When two files share a name, the second goes in as `name (1).ext`, and so on.

use std::collections::HashMap;
use std::io::Write as _;

/// The zip's name, from the subject and the message's place in the conversation. The subject is
/// lowercased, each run of one or two spaces becomes an underscore, and anything else that isn't a
/// letter, digit or underscore is dropped. The place follows, counting from 1. Without a subject
/// the name is `attachments.zip`.
pub(crate) fn name(subject: &str, position: usize) -> String {
    let mut slug = String::new();
    let mut chars = subject.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            // A second space goes with the first, so two make one underscore.
            chars.next_if(|next| next.is_whitespace());
            slug.push('_');
        } else if c.is_alphanumeric() || c == '_' {
            slug.extend(c.to_lowercase());
        }
    }
    if slug.trim_matches('_').is_empty() {
        return "attachments.zip".into();
    }
    format!("{slug}_{}.zip", position + 1)
}

/// The names the files go in under. Each keeps its name, except that a name already taken gets
/// ` (n)` before its extension, where n counts how often that name came up.
fn unique(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut unique_name = |name: String| -> String {
        let mut name = name;
        loop {
            let Some(count) = seen.get(&name).copied() else {
                seen.insert(name.clone(), 1);
                return name;
            };
            seen.insert(name.clone(), count + 1);
            name = match name.rfind('.').filter(|dot| *dot > 0) {
                Some(dot) => format!("{} ({count}){}", &name[..dot], &name[dot..]),
                None => format!("{name} ({count})"),
            };
        }
    };
    names.into_iter().map(&mut unique_name).collect()
}

/// `files` (name and bytes) as a zip, deflated, dated `now` (DOS time). Names are made safe for a
/// file system first, as downloads' are.
pub(crate) fn archive(files: &[(String, Vec<u8>)], now: &jiff::civil::DateTime) -> Vec<u8> {
    let (time, date) = dos_time(now);
    let names = unique(files.iter().map(|(name, _)| super::file_name(name)));
    let mut out = Vec::new();
    let mut central = Vec::new();
    let mut any_zip64 = false;
    for ((_, data), raw_name) in files.iter().zip(&names) {
        let name = &raw_name[..raw_name.floor_char_boundary(usize::from(u16::MAX))];
        let mut encoder = flate2::CrcWriter::new(flate2::write::DeflateEncoder::new(
            Vec::new(),
            flate2::Compression::default(),
        ));
        encoder
            .write_all(data)
            .expect("deflating into memory doesn't fail");
        let crc = encoder.crc().sum();
        let compressed = encoder
            .into_inner()
            .finish()
            .expect("deflating into memory doesn't fail");
        let offset = out.len();
        let fields = Entry {
            crc,
            compressed: compressed.len(),
            size: data.len(),
            offset,
            time,
            date,
            name,
        };
        any_zip64 |= fields.needs_zip64();
        fields.write_local(&mut out);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&compressed);
        fields.write_central(&mut central);
    }
    let (start, length) = (out.len(), central.len());
    out.extend_from_slice(&central);
    let zip64 = any_zip64
        || u16::try_from(files.len()).is_err()
        || u32::try_from(length).is_err()
        || u32::try_from(start).is_err();
    if zip64 {
        let eocd64_offset = out.len() as u64;
        let entries = files.len() as u64;
        // ZIP64 end of central directory record (56 bytes).
        out.extend_from_slice(&0x0606_4b50_u32.to_le_bytes());
        out.extend_from_slice(&44_u64.to_le_bytes());
        out.extend_from_slice(&45_u16.to_le_bytes());
        out.extend_from_slice(&45_u16.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&entries.to_le_bytes());
        out.extend_from_slice(&entries.to_le_bytes());
        out.extend_from_slice(&(length as u64).to_le_bytes());
        out.extend_from_slice(&(start as u64).to_le_bytes());
        // ZIP64 end of central directory locator (20 bytes).
        out.extend_from_slice(&0x0706_4b50_u32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&eocd64_offset.to_le_bytes());
        out.extend_from_slice(&1_u32.to_le_bytes());
    }
    // End of central directory.
    let count = u16::try_from(files.len()).unwrap_or(u16::MAX).to_le_bytes();
    out.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&count);
    out.extend_from_slice(&count);
    out.extend_from_slice(&u32::try_from(length).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(&u32::try_from(start).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(&[0; 2]);
    out
}

/// Header fields for one archived file.
struct Entry<'a> {
    crc: u32,
    compressed: usize,
    size: usize,
    offset: usize,
    time: u16,
    date: u16,
    name: &'a str,
}

impl Entry<'_> {
    fn needs_zip64(&self) -> bool {
        u32::try_from(self.size).is_err()
            || u32::try_from(self.compressed).is_err()
            || u32::try_from(self.offset).is_err()
    }

    fn write_local(&self, out: &mut Vec<u8>) {
        let local_64 = u32::try_from(self.size).is_err() || u32::try_from(self.compressed).is_err();
        let version: u16 = if local_64 { 45 } else { 20 };
        let (compressed, size, extra_len): (u32, u32, u16) = if local_64 {
            (u32::MAX, u32::MAX, 20)
        } else {
            (self.compressed as u32, self.size as u32, 0)
        };
        out.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
        out.extend_from_slice(&version.to_le_bytes());
        out.extend_from_slice(&0x0800_u16.to_le_bytes());
        out.extend_from_slice(&8_u16.to_le_bytes());
        out.extend_from_slice(&self.time.to_le_bytes());
        out.extend_from_slice(&self.date.to_le_bytes());
        out.extend_from_slice(&self.crc.to_le_bytes());
        out.extend_from_slice(&compressed.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&(self.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&extra_len.to_le_bytes());
        if local_64 {
            out.extend_from_slice(&0x0001_u16.to_le_bytes());
            out.extend_from_slice(&16_u16.to_le_bytes());
            out.extend_from_slice(&(self.size as u64).to_le_bytes());
            out.extend_from_slice(&(self.compressed as u64).to_le_bytes());
        }
    }

    fn write_central(&self, central: &mut Vec<u8>) {
        let mut extra_data = Vec::new();
        let size = match u32::try_from(self.size) {
            Ok(v) => v,
            Err(_) => {
                extra_data.extend_from_slice(&(self.size as u64).to_le_bytes());
                u32::MAX
            }
        };
        let compressed = match u32::try_from(self.compressed) {
            Ok(v) => v,
            Err(_) => {
                extra_data.extend_from_slice(&(self.compressed as u64).to_le_bytes());
                u32::MAX
            }
        };
        let offset = match u32::try_from(self.offset) {
            Ok(v) => v,
            Err(_) => {
                extra_data.extend_from_slice(&(self.offset as u64).to_le_bytes());
                u32::MAX
            }
        };
        let version: u16 = if extra_data.is_empty() { 20 } else { 45 };
        let extra_len: u16 = if extra_data.is_empty() {
            0
        } else {
            4 + extra_data.len() as u16
        };
        central.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
        central.extend_from_slice(&version.to_le_bytes());
        central.extend_from_slice(&version.to_le_bytes());
        central.extend_from_slice(&0x0800_u16.to_le_bytes());
        central.extend_from_slice(&8_u16.to_le_bytes());
        central.extend_from_slice(&self.time.to_le_bytes());
        central.extend_from_slice(&self.date.to_le_bytes());
        central.extend_from_slice(&self.crc.to_le_bytes());
        central.extend_from_slice(&compressed.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&(self.name.len() as u16).to_le_bytes());
        central.extend_from_slice(&extra_len.to_le_bytes());
        // No comment, disk 0, no internal or external attributes, then where the local header is.
        central.extend_from_slice(&[0; 10]);
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(self.name.as_bytes());
        if !extra_data.is_empty() {
            central.extend_from_slice(&0x0001_u16.to_le_bytes());
            central.extend_from_slice(&(extra_data.len() as u16).to_le_bytes());
            central.extend_from_slice(&extra_data);
        }
    }
}

/// `at` as MS-DOS's time and date, to the even second.
fn dos_time(at: &jiff::civil::DateTime) -> (u16, u16) {
    let time = (u16::try_from(at.hour()).unwrap_or(0) << 11)
        | (u16::try_from(at.minute()).unwrap_or(0) << 5)
        | (u16::try_from(at.second()).unwrap_or(0) / 2);
    let year_offset = i32::from(at.year()).saturating_sub(1980).clamp(0, 127) as u16;
    let date = (year_offset << 9)
        | (u16::try_from(at.month()).unwrap_or(1) << 5)
        | u16::try_from(at.day()).unwrap_or(1);
    (time, date)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_zip_is_named_for_the_conversation() {
        assert_eq!(name("Board deck — draft 2", 2), "board_deck__draft_2_3.zip");
        assert_eq!(
            name("Q3  numbers", 0),
            "q3_numbers_1.zip",
            "two spaces make one underscore"
        );
        assert_eq!(name("請求書 2026", 0), "請求書_2026_1.zip");
        assert_eq!(name("Отчёт Q3", 1), "отчёт_q3_2.zip");
        assert_eq!(name("🎉 🚀", 0), "attachments.zip");
        assert_eq!(name("🎉", 0), "attachments.zip");
        assert_eq!(name("", 0), "attachments.zip");
    }

    #[test]
    fn a_name_taken_gets_a_number() {
        let names =
            unique(["deck.pdf", "deck.pdf", "deck.pdf", "deck (1).pdf", "notes"].map(String::from));
        assert_eq!(
            names,
            [
                "deck.pdf",
                "deck (1).pdf",
                "deck (2).pdf",
                "deck (1) (1).pdf",
                "notes"
            ]
        );
    }

    #[test]
    fn the_archive_opens() {
        let files = vec![
            ("a.txt".to_owned(), b"hello hello hello".to_vec()),
            ("a.txt".to_owned(), b"second".to_vec()),
        ];
        let zip = archive(&files, &jiff::civil::date(2026, 11, 17).at(9, 30, 4, 0));
        let dir = std::env::temp_dir().join(format!("exo-zip-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a folder");
        let path = dir.join("files.zip");
        std::fs::write(&path, &zip).expect("written");
        // `unzip` reads it back whole, checking each file's CRC.
        let status = std::process::Command::new("unzip")
            .arg("-q")
            .arg(&path)
            .arg("-d")
            .arg(&dir)
            .status()
            .expect("unzip runs");
        assert!(status.success());
        assert_eq!(
            std::fs::read(dir.join("a.txt")).expect("first"),
            b"hello hello hello"
        );
        assert_eq!(
            std::fs::read(dir.join("a (1).txt")).expect("second"),
            b"second"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
