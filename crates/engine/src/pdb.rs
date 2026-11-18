//! Writes `PIONEER/rekordbox/export.pdb`, the database CDJ and XDJ players read from a USB stick.
//!
//! The format was reverse-engineered by Deep Symmetry, Henry Betts, Fabian Lesniak and others
//! (<https://djl-analysis.deepsymmetry.org/rekordbox-export-analysis/exports.html>). Everything is little
//! endian. The file is a header page followed by pages of 4096 bytes; each table is a chain of pages:
//!
//! * an *index page* that lists the table's data pages,
//! * data pages holding rows. Rows grow forward from byte 0x28, and a row index (groups of up to 16
//!   offsets plus a presence mask) grows backward from the end of the page.
//!
//! Tables Dubplate does not fill (artwork, history…) are written empty, and the handful of tables whose
//! contents never change between exports (colours, browse categories, menu layout) come from
//! [`crate::pdb_fixed`].
//!
//! Without a CDJ to hand, the output is checked by parsing it back with the `rekordcrate` library and,
//! during development, byte for byte against an export made by rekordbox itself.

use crate::pdb_fixed;

const PAGE: usize = 4096;
const HEAP: usize = 0x28;
const NUM_TABLES: u32 = 20;
/// The index page holds this many 4-byte slots; unused ones read `EMPTY_SLOT`.
const INDEX_SLOTS: usize = 1004;
const EMPTY_SLOT: u32 = 0x1fff_fff8;
const NO_PAGE: u32 = 0x03ff_ffff;

/// Table type numbers as they appear in the header and in every page.
mod ty {
    pub const TRACKS: u32 = 0;
    pub const GENRES: u32 = 1;
    pub const ARTISTS: u32 = 2;
    pub const ALBUMS: u32 = 3;
    #[allow(dead_code)]
    pub const LABELS: u32 = 4;
    pub const KEYS: u32 = 5;
    pub const COLORS: u32 = 6;
    pub const PLAYLIST_TREE: u32 = 7;
    pub const PLAYLIST_ENTRIES: u32 = 8;
    pub const COLUMNS: u32 = 16;
    pub const MENU_17: u32 = 17;
    pub const MENU_18: u32 = 18;
    pub const HISTORY: u32 = 19;
}

/// A track as it goes on the stick.
#[derive(Clone, Debug, Default)]
pub struct ExportTrack {
    /// Unique, 1-based.
    pub id: u32,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    /// rekordbox spelling, e.g. "Am" or "C#" (see [`key_name`]).
    pub key: Option<String>,
    pub bpm: f32,
    pub duration_secs: u32,
    pub file_size: u32,
    pub sample_rate: u32,
    pub bitrate_kbps: u32,
    pub bit_depth: u16,
    pub year: u16,
    pub track_number: u32,
    pub comment: String,
    /// "YYYY-MM-DD"
    pub date_added: String,
    /// Where the audio sits on the stick, e.g. "/Contents/Artist/Album/Song.mp3".
    pub file_path: String,
    /// Where its analysis file sits, e.g. "/PIONEER/USBANLZ/P016/0000875E/ANLZ0000.DAT".
    pub analyze_path: String,
    /// "YYYY-MM-DD"
    pub analyze_date: String,
}

/// A playlist (a Dubplate reel) and the ids of its tracks, in order.
#[derive(Clone, Debug)]
pub struct ExportPlaylist {
    pub name: String,
    pub track_ids: Vec<u32>,
}

/// rekordbox's name for a key: minor keys are lower-case with an "m" ("Am"), majors plain ("C").
/// `pitch_class` is 0 = C … 11 = B.
pub fn key_name(pitch_class: u8, minor: bool) -> String {
    const NAMES: [&str; 12] = ["C", "Db", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"];
    format!("{}{}", NAMES[(pitch_class % 12) as usize], if minor { "m" } else { "" })
}

/// File type code stored in the track row.
pub fn file_type(path: &str) -> u16 {
    match path.rsplit('.').next().map(str::to_ascii_lowercase).as_deref() {
        Some("mp3") => 0x01,
        Some("m4a") | Some("aac") | Some("mp4") => 0x04,
        Some("flac") => 0x05,
        Some("wav") => 0x0b,
        Some("aif") | Some("aiff") => 0x0c,
        _ => 0x00,
    }
}

// ── primitive encoders ───────────────────────────────────────────────────────

fn u16le(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn u32le(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

/// A DeviceSQL string: short ASCII (up to 126 bytes) is a length byte then the text; anything else is
/// a flag byte, a 16-bit length that counts the 4 header bytes, a pad byte and the text (ASCII or
/// UTF-16LE). There is no terminator.
pub fn dsql(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    if s.is_ascii() && s.len() <= 126 {
        out.push((((s.len() + 1) << 1) | 1) as u8);
        out.extend_from_slice(s.as_bytes());
    } else if s.is_ascii() {
        let s = &s[..s.len().min(0x7ff0)];
        out.push(0x40);
        u16le(&mut out, s.len() as u16 + 4);
        out.push(0);
        out.extend_from_slice(s.as_bytes());
    } else {
        let units: Vec<u16> = s.encode_utf16().take(0x3ff0).collect();
        out.push(0x90);
        u16le(&mut out, units.len() as u16 * 2 + 4);
        out.push(0);
        for u in units {
            u16le(&mut out, u);
        }
    }
    out
}

/// Rows start on a 4-byte boundary.
fn pad4(v: &mut Vec<u8>) {
    while !v.len().is_multiple_of(4) {
        v.push(0);
    }
}

fn named_row(id: u32, name: &str) -> Vec<u8> {
    let mut r = Vec::new();
    u32le(&mut r, id);
    r.extend(dsql(name));
    pad4(&mut r);
    r
}

/// `index_shift` (offset 2) is filled in when the row lands on a page: 0x20 times its position there.
fn artist_row(id: u32, name: &str) -> Vec<u8> {
    let mut r = vec![0x60, 0x00, 0, 0];
    u32le(&mut r, id);
    r.extend_from_slice(&[0x03, 0x0a]);
    r.extend(dsql(name));
    pad4(&mut r);
    r
}

fn album_row(id: u32, artist_id: u32, name: &str) -> Vec<u8> {
    let mut r = vec![0x80, 0x00, 0, 0];
    u32le(&mut r, 0);
    u32le(&mut r, artist_id);
    u32le(&mut r, id);
    u32le(&mut r, 0);
    r.extend_from_slice(&[0x03, 0x16]);
    r.extend(dsql(name));
    pad4(&mut r);
    r
}

fn key_row(id: u32, name: &str) -> Vec<u8> {
    let mut r = Vec::new();
    u32le(&mut r, id);
    u32le(&mut r, id);
    r.extend(dsql(name));
    pad4(&mut r);
    r
}

fn playlist_row(id: u32, sort: u32, name: &str) -> Vec<u8> {
    let mut r = Vec::new();
    u32le(&mut r, 0); // parent: the root
    u32le(&mut r, 0);
    u32le(&mut r, sort);
    u32le(&mut r, id);
    u32le(&mut r, 0); // not a folder
    r.extend(dsql(name));
    pad4(&mut r);
    r
}

fn entry_row(index: u32, track: u32, playlist: u32) -> Vec<u8> {
    let mut r = Vec::new();
    u32le(&mut r, index);
    u32le(&mut r, track);
    u32le(&mut r, playlist);
    r
}

/// Ids of the lookup rows a track points at (0 = none).
struct Refs {
    artist: u32,
    album: u32,
    genre: u32,
    key: u32,
}

fn track_row(t: &ExportTrack, r: &Refs) -> Vec<u8> {
    let mut v = vec![0x24, 0x00, 0, 0]; // subtype, index_shift
    u32le(&mut v, 0x000c_0700);
    u32le(&mut v, t.sample_rate);
    u32le(&mut v, 0); // composer
    u32le(&mut v, t.file_size);
    u32le(&mut v, path_hash(&t.file_path) & 0x0fff_ffff);
    u16le(&mut v, 0xfa80);
    u16le(&mut v, 0x05e7);
    u32le(&mut v, 0); // artwork
    u32le(&mut v, r.key);
    u32le(&mut v, 0); // original artist
    u32le(&mut v, 0); // label
    u32le(&mut v, 0); // remixer
    u32le(&mut v, t.bitrate_kbps);
    u32le(&mut v, t.track_number);
    u32le(&mut v, (t.bpm * 100.0).round().max(0.0) as u32);
    u32le(&mut v, r.genre);
    u32le(&mut v, r.album);
    u32le(&mut v, r.artist);
    u32le(&mut v, t.id);
    u16le(&mut v, 0); // disc
    u16le(&mut v, 0); // play count
    u16le(&mut v, t.year);
    u16le(&mut v, if t.bit_depth == 0 { 16 } else { t.bit_depth });
    u16le(&mut v, t.duration_secs.min(0xffff) as u16);
    u16le(&mut v, 41);
    v.push(0); // colour
    v.push(0); // rating
    u16le(&mut v, file_type(&t.file_path));
    u16le(&mut v, 3);
    debug_assert_eq!(v.len(), 0x5e);

    // 21 string offsets, then the strings in the same order.
    let none = String::new();
    let strings: [&str; 21] = [
        &none,
        &none,
        "3",
        "3",
        &none,
        &none,
        &none,
        "ON",
        &none,
        &none,
        &t.date_added,
        &none,
        &none,
        &none,
        &t.analyze_path,
        &t.analyze_date,
        &t.comment,
        &t.title,
        &none,
        file_name(&t.file_path),
        &t.file_path,
    ];
    let mut offsets = Vec::with_capacity(21);
    let mut body = Vec::new();
    for s in strings {
        offsets.push((0x88 + body.len()) as u16);
        body.extend(dsql(s));
    }
    for o in offsets {
        u16le(&mut v, o);
    }
    v.extend(body);
    pad4(&mut v);
    v
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// A stable 32-bit hash (FNV-1a); players only need the field to differ between tracks.
fn path_hash(s: &str) -> u32 {
    s.bytes().fold(0x811c_9dc5u32, |h, b| (h ^ b as u32).wrapping_mul(0x0100_0193))
}

// ── pages ────────────────────────────────────────────────────────────────────

struct Table {
    typ: u32,
    rows: Vec<Vec<u8>>,
    /// Rows carry an `index_shift` at offset 2 that depends on where they land.
    shift: bool,
}

/// Bytes the row index of `n` rows needs at the end of a page.
fn footer_len(n: usize) -> usize {
    2 * n + 4 * n.div_ceil(16)
}

/// Splits rows over pages: each entry is the rows of one page.
fn split_pages(rows: &[Vec<u8>]) -> Vec<Vec<&Vec<u8>>> {
    let mut pages: Vec<Vec<&Vec<u8>>> = Vec::new();
    let mut used = 0usize;
    for r in rows {
        let fits = pages.last().is_some_and(|p| HEAP + used + r.len() + footer_len(p.len() + 1) <= PAGE);
        if !fits {
            pages.push(Vec::new());
            used = 0;
        }
        pages.last_mut().unwrap().push(r);
        used += r.len();
    }
    pages
}

/// One data page. `seq` is the page's sequence number.
fn data_page(index: u32, typ: u32, next: u32, seq: u32, rows: &[&Vec<u8>], shift: bool) -> Vec<u8> {
    let mut p = vec![0u8; PAGE];
    let put32 = |p: &mut Vec<u8>, at: usize, x: u32| p[at..at + 4].copy_from_slice(&x.to_le_bytes());
    let put16 = |p: &mut Vec<u8>, at: usize, x: u16| p[at..at + 2].copy_from_slice(&x.to_le_bytes());
    put32(&mut p, 4, index);
    put32(&mut p, 8, typ);
    put32(&mut p, 12, next);
    put32(&mut p, 16, seq);

    let n = rows.len();
    let packed = (n as u32 & 0x1fff) | ((n as u32) << 13);
    p[24..27].copy_from_slice(&packed.to_le_bytes()[..3]);
    p[27] = 0x24;

    let mut heap = HEAP;
    let mut offsets = Vec::with_capacity(n);
    for (i, r) in rows.iter().enumerate() {
        offsets.push((heap - HEAP) as u16);
        let at = heap;
        p[at..at + r.len()].copy_from_slice(r);
        if shift {
            put16(&mut p, at + 2, (0x20 * i) as u16);
        }
        heap += r.len();
    }
    let used = heap - HEAP;
    let free = PAGE - HEAP - used - footer_len(n);
    put16(&mut p, 28, free as u16);
    put16(&mut p, 30, used as u16);
    put16(&mut p, 32, n as u16); // rows in the last transaction: all of them
                                 // A one-byte row count at 0x18 cannot hold 256 or more; readers then look here.
    put16(&mut p, 34, if n > 255 { n as u16 } else { 0 });

    for g in 0..n.div_ceil(16) {
        let end = PAGE - 36 * g;
        let count = (n - 16 * g).min(16);
        for k in 0..count {
            put16(&mut p, end - 4 - 2 * (k + 1), offsets[16 * g + k]);
        }
        let mask = ((1u32 << count) - 1) as u16;
        put16(&mut p, end - 4, mask);
        put16(&mut p, end - 2, mask);
    }
    p
}

/// The first page of a table. Its slot list names pages that hold deleted rows, which a fresh export
/// has none of, so every slot stays empty. `first_data` is the table's first page of rows, if any.
fn index_page(index: u32, typ: u32, next: u32, first_data: Option<u32>) -> Vec<u8> {
    let mut p = vec![0u8; PAGE];
    let put32 = |p: &mut Vec<u8>, at: usize, x: u32| p[at..at + 4].copy_from_slice(&x.to_le_bytes());
    let put16 = |p: &mut Vec<u8>, at: usize, x: u16| p[at..at + 2].copy_from_slice(&x.to_le_bytes());
    put32(&mut p, 4, index);
    put32(&mut p, 8, typ);
    put32(&mut p, 12, next);
    put32(&mut p, 16, 1);
    p[27] = 0x64;
    put16(&mut p, 0x20, 0x1fff);
    put16(&mut p, 0x22, 0x1fff);
    put16(&mut p, 0x24, 0x03ec);
    put32(&mut p, 0x28, index);
    put32(&mut p, 0x2c, first_data.unwrap_or(NO_PAGE));
    put32(&mut p, 0x30, NO_PAGE);
    put16(&mut p, 0x3a, 0x1fff);
    for k in 0..INDEX_SLOTS {
        put32(&mut p, 0x3c + 4 * k, EMPTY_SLOT);
    }
    p
}

fn hex_rows(rows: &[&str]) -> Vec<Vec<u8>> {
    rows.iter()
        .map(|h| (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).expect("hex")).collect())
        .collect()
}

/// Builds the complete file.
pub fn build_export_pdb(tracks: &[ExportTrack], playlists: &[ExportPlaylist]) -> Vec<u8> {
    // Lookup tables: ids are handed out in order of first use.
    fn intern(list: &mut Vec<String>, s: &str) -> u32 {
        if s.trim().is_empty() {
            return 0;
        }
        match list.iter().position(|x| x == s) {
            Some(i) => i as u32 + 1,
            None => {
                list.push(s.to_owned());
                list.len() as u32
            }
        }
    }
    let (mut genres, mut artists, mut keys) = (Vec::<String>::new(), Vec::<String>::new(), Vec::<String>::new());
    let mut albums: Vec<(String, u32)> = Vec::new();
    let mut track_rows = Vec::with_capacity(tracks.len());
    for t in tracks {
        let artist = intern(&mut artists, &t.artist);
        let album = if t.album.trim().is_empty() {
            0
        } else {
            match albums.iter().position(|(n, a)| *n == t.album && *a == artist) {
                Some(i) => i as u32 + 1,
                None => {
                    albums.push((t.album.clone(), artist));
                    albums.len() as u32
                }
            }
        };
        let genre = intern(&mut genres, &t.genre);
        let key = t.key.as_deref().map_or(0, |k| intern(&mut keys, k));
        track_rows.push(track_row(t, &Refs { artist, album, genre, key }));
    }

    let mut tables: Vec<Table> = (0..NUM_TABLES).map(|typ| Table { typ, rows: Vec::new(), shift: false }).collect();
    let set = |tables: &mut Vec<Table>, typ: u32, rows: Vec<Vec<u8>>, shift: bool| {
        tables[typ as usize].rows = rows;
        tables[typ as usize].shift = shift;
    };
    set(&mut tables, ty::TRACKS, track_rows, true);
    set(&mut tables, ty::GENRES, genres.iter().enumerate().map(|(i, n)| named_row(i as u32 + 1, n)).collect(), false);
    set(&mut tables, ty::ARTISTS, artists.iter().enumerate().map(|(i, n)| artist_row(i as u32 + 1, n)).collect(), true);
    set(
        &mut tables,
        ty::ALBUMS,
        albums.iter().enumerate().map(|(i, (n, a))| album_row(i as u32 + 1, *a, n)).collect(),
        true,
    );
    set(&mut tables, ty::KEYS, keys.iter().enumerate().map(|(i, n)| key_row(i as u32 + 1, n)).collect(), false);
    set(&mut tables, ty::COLORS, hex_rows(pdb_fixed::COLORS), false);
    set(
        &mut tables,
        ty::PLAYLIST_TREE,
        playlists.iter().enumerate().map(|(i, p)| playlist_row(i as u32 + 1, i as u32, &p.name)).collect(),
        false,
    );
    set(
        &mut tables,
        ty::PLAYLIST_ENTRIES,
        playlists
            .iter()
            .enumerate()
            .flat_map(|(i, p)| {
                p.track_ids.iter().enumerate().map(move |(n, &t)| entry_row(n as u32 + 1, t, i as u32 + 1))
            })
            .collect(),
        false,
    );
    set(&mut tables, ty::COLUMNS, hex_rows(pdb_fixed::COLUMNS), false);
    set(&mut tables, ty::MENU_17, hex_rows(pdb_fixed::MENU_17), false);
    set(&mut tables, ty::MENU_18, hex_rows(pdb_fixed::MENU_18), false);
    set(&mut tables, ty::HISTORY, hex_rows(pdb_fixed::HISTORY), false);

    // First pass: how many pages does each table take? An empty table is an index page and a blank one.
    let split: Vec<Vec<Vec<&Vec<u8>>>> = tables.iter().map(|t| split_pages(&t.rows)).collect();
    let real_pages: u32 = 1 + split.iter().map(|s| if s.is_empty() { 2 } else { 1 + s.len() as u32 }).sum::<u32>();
    // Populated tables end their chain on a spare page number past the end of the file.
    let populated = split.iter().filter(|s| !s.is_empty()).count() as u32;
    // rekordbox hands the history table the first spare number, then the rest in table order.
    let spare_of = |typ: u32| -> u32 {
        let order = split.iter().zip(&tables).filter(|(s, _)| !s.is_empty()).map(|(_, t)| t.typ);
        let mut order: Vec<u32> = order.collect();
        order.sort_by_key(|&t| (t != ty::HISTORY, t));
        real_pages + order.iter().position(|&t| t == typ).unwrap() as u32
    };
    let next_unused = real_pages + populated;

    let mut pages: Vec<Vec<u8>> = vec![Vec::new()]; // page 0 is filled last
    let mut pointers: Vec<(u32, u32, u32, u32)> = Vec::new(); // type, empty candidate, first, last
    let mut seq = 1u32;
    for (t, parts) in tables.iter().zip(&split) {
        let index = pages.len() as u32;
        if parts.is_empty() {
            pages.push(index_page(index, t.typ, index + 1, None));
            pages.push(vec![0u8; PAGE]);
            pointers.push((t.typ, index + 1, index, index));
            continue;
        }
        let first_data = index + 1;
        pages.push(index_page(index, t.typ, first_data, Some(first_data)));
        let candidate = spare_of(t.typ);
        for (k, rows) in parts.iter().enumerate() {
            // The history table was written first by rekordbox; its page keeps sequence number 1.
            let page_seq = if t.typ == ty::HISTORY {
                1
            } else {
                seq += 1;
                seq
            };
            let next = if k + 1 < parts.len() { first_data + k as u32 + 1 } else { candidate };
            pages.push(data_page(first_data + k as u32, t.typ, next, page_seq, rows, t.shift));
        }
        pointers.push((t.typ, candidate, index, first_data + parts.len() as u32 - 1));
    }

    let mut head = vec![0u8; PAGE];
    head[4..8].copy_from_slice(&(PAGE as u32).to_le_bytes());
    head[8..12].copy_from_slice(&NUM_TABLES.to_le_bytes());
    head[12..16].copy_from_slice(&next_unused.to_le_bytes());
    head[16..20].copy_from_slice(&5u32.to_le_bytes());
    head[20..24].copy_from_slice(&(seq + 1).to_le_bytes());
    for (i, (typ, cand, first, last)) in pointers.iter().enumerate() {
        let at = 0x1c + 16 * i;
        for (k, v) in [typ, cand, first, last].into_iter().enumerate() {
            head[at + 4 * k..at + 4 * k + 4].copy_from_slice(&v.to_le_bytes());
        }
    }
    pages[0] = head;
    pages.concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: u32, title: &str, artist: &str, album: &str, key: Option<&str>) -> ExportTrack {
        ExportTrack {
            id,
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            genre: "House".into(),
            key: key.map(str::to_owned),
            bpm: 124.0,
            duration_secs: 245,
            file_size: 9_800_000,
            sample_rate: 44100,
            bitrate_kbps: 320,
            bit_depth: 16,
            year: 2021,
            track_number: 3,
            comment: "".into(),
            date_added: "2026-10-23".into(),
            file_path: format!("/Contents/{artist}/{album}/{title}.mp3"),
            analyze_path: format!("/PIONEER/USBANLZ/P001/{id:08X}/ANLZ0000.DAT"),
            analyze_date: "2026-10-23".into(),
        }
    }

    #[test]
    fn strings_use_the_device_encoding() {
        assert_eq!(dsql(""), vec![0x03]);
        assert_eq!(dsql("foo"), vec![0x09, b'f', b'o', b'o']);
        let long = "x".repeat(200);
        let e = dsql(&long);
        assert_eq!((e[0], u16::from_le_bytes([e[1], e[2]]), e[3]), (0x40, 204, 0));
        let u = dsql("Bjørk");
        assert_eq!(u[0], 0x90);
        assert_eq!(u16::from_le_bytes([u[1], u[2]]), 5 * 2 + 4);
    }

    #[test]
    fn keys_and_file_types() {
        assert_eq!(key_name(9, true), "Am");
        assert_eq!(key_name(0, false), "C");
        assert_eq!(key_name(6, false), "F#");
        assert_eq!(file_type("/a/b.MP3"), 1);
        assert_eq!(file_type("x.flac"), 5);
        assert_eq!(file_type("x.wav"), 11);
        assert_eq!(file_type("x.ogg"), 0);
    }

    #[test]
    fn the_file_is_whole_pages() {
        let f = build_export_pdb(&[], &[]);
        assert_eq!(f.len() % PAGE, 0);
        assert_eq!(&f[4..8], &4096u32.to_le_bytes());
        let many: Vec<_> = (1..=300).map(|i| track(i, &format!("Song {i}"), "Artist", "Album", Some("Am"))).collect();
        let f = build_export_pdb(&many, &[]);
        assert_eq!(f.len() % PAGE, 0);
        // The header says where unused pages start: past everything that exists.
        let next_unused = u32::from_le_bytes(f[12..16].try_into().unwrap());
        assert!(next_unused as usize > f.len() / PAGE);
    }

    #[test]
    fn many_rows_spread_over_pages() {
        let rows: Vec<Vec<u8>> = (0..500).map(|i| named_row(i, "Some genre")).collect();
        let pages = split_pages(&rows);
        assert!(pages.len() > 1);
        assert_eq!(pages.iter().map(Vec::len).sum::<usize>(), 500);
        for p in &pages {
            let bytes: usize = p.iter().map(|r| r.len()).sum();
            assert!(HEAP + bytes + footer_len(p.len()) <= PAGE);
        }
    }
}

/// Reads our output back with rekordcrate, an independent reader written against real exports.
#[cfg(test)]
mod reread {
    use super::*;
    use binrw::BinRead;
    use rekordcrate::pdb::{Header, PageType, Row};
    use std::io::Cursor;

    fn read(file: &[u8]) -> (Header, Vec<(PageType, Vec<Row>)>) {
        let mut cur = Cursor::new(file);
        let header = Header::read(&mut cur).expect("header");
        let mut tables = Vec::new();
        for t in &header.tables {
            let pages =
                header.read_pages(&mut cur, binrw::Endian::Little, (&t.first_page, &t.last_page)).expect("pages");
            let rows: Vec<Row> = pages
                .iter()
                .filter(|p| p.has_data())
                .flat_map(|p| p.row_groups.iter().flat_map(|g| g.present_rows()))
                .collect();
            tables.push((t.page_type, rows));
        }
        (header, tables)
    }

    fn rows_of(tables: &[(PageType, Vec<Row>)], t: PageType) -> &Vec<Row> {
        &tables.iter().find(|(p, _)| *p == t).unwrap().1
    }

    fn sample() -> (Vec<ExportTrack>, Vec<ExportPlaylist>) {
        let mk = |i: u32, t: &str, a: &str, al: &str, k: Option<&str>| ExportTrack {
            id: i,
            title: t.into(),
            artist: a.into(),
            album: al.into(),
            genre: "House".into(),
            key: k.map(str::to_owned),
            bpm: 124.5,
            duration_secs: 245,
            file_size: 9_800_000,
            sample_rate: 44100,
            bitrate_kbps: 320,
            bit_depth: 16,
            year: 2021,
            track_number: 3,
            comment: "Nice one".into(),
            date_added: "2026-10-23".into(),
            file_path: format!("/Contents/{a}/{al}/{t}.mp3"),
            analyze_path: format!("/PIONEER/USBANLZ/P001/{i:08X}/ANLZ0000.DAT"),
            analyze_date: "2026-10-23".into(),
        };
        (
            vec![
                mk(1, "Levels", "Avicii", "Levels", Some("Am")),
                mk(2, "Strobe", "deadmau5", "For Lack of a Better Name", Some("C")),
                mk(3, "Second Song", "Avicii", "Levels", None),
            ],
            vec![
                ExportPlaylist { name: "Friday set".into(), track_ids: vec![2, 1] },
                ExportPlaylist { name: "Warm up".into(), track_ids: vec![3] },
            ],
        )
    }

    #[test]
    fn an_empty_export_has_every_table_and_the_fixed_rows() {
        let (h, tables) = read(&build_export_pdb(&[], &[]));
        assert_eq!(h.page_size, 4096);
        assert_eq!(h.tables.len(), 20);
        assert_eq!(rows_of(&tables, PageType::Colors).len(), 8);
        assert_eq!(rows_of(&tables, PageType::Columns).len(), 27);
        assert!(rows_of(&tables, PageType::Tracks).is_empty());
    }

    #[test]
    fn tracks_and_lookups_read_back() {
        let (tracks, lists) = sample();
        let (_, tables) = read(&build_export_pdb(&tracks, &lists));
        assert_eq!(rows_of(&tables, PageType::Tracks).len(), 3);
        assert!(rows_of(&tables, PageType::Tracks).iter().all(|r| matches!(r, Row::Track(_))));
        assert_eq!(rows_of(&tables, PageType::Artists).len(), 2, "Avicii is listed once");
        assert_eq!(rows_of(&tables, PageType::Albums).len(), 2);
        assert_eq!(rows_of(&tables, PageType::Genres).len(), 1);
        assert_eq!(rows_of(&tables, PageType::Keys).len(), 2);
        assert_eq!(rows_of(&tables, PageType::PlaylistTree).len(), 2);
        assert_eq!(rows_of(&tables, PageType::PlaylistEntries).len(), 3);
    }

    #[test]
    fn names_survive_including_non_ascii() {
        let (mut tracks, lists) = sample();
        tracks[0].title = "Café del Mar — Ørsted".into();
        tracks[1].artist = "A".repeat(300);
        let (_, tables) = read(&build_export_pdb(&tracks, &lists));
        let dump = format!("{:?}", rows_of(&tables, PageType::Tracks));
        assert!(dump.contains("Café del Mar — Ørsted"), "{dump}");
        let artists = format!("{:?}", rows_of(&tables, PageType::Artists));
        assert!(artists.contains(&"A".repeat(300)));
    }

    #[test]
    fn playlists_keep_their_order() {
        let (tracks, lists) = sample();
        let (_, tables) = read(&build_export_pdb(&tracks, &lists));
        let entries: Vec<String> =
            rows_of(&tables, PageType::PlaylistEntries).iter().map(|r| format!("{r:?}")).collect();
        let has = |idx: u32, track: u32, list: u32| {
            entries.iter().any(|e| {
                e.contains(&format!("entry_index: {idx},"))
                    && e.contains(&format!("TrackId({track})"))
                    && e.contains(&format!("PlaylistTreeNodeId({list})"))
            })
        };
        // Friday set is playlist 1: track 2, then track 1. Warm up is playlist 2: track 3.
        assert!(has(1, 2, 1) && has(2, 1, 1) && has(1, 3, 2), "{entries:#?}");
        let tree = format!("{:?}", rows_of(&tables, PageType::PlaylistTree));
        assert!(tree.contains("Friday set") && tree.contains("Warm up"));
    }

    #[test]
    fn a_large_library_fills_several_pages() {
        let (tracks, _) = sample();
        let many: Vec<ExportTrack> = (1..=2000)
            .map(|i| {
                let mut t = tracks[(i % 3) as usize].clone();
                t.id = i;
                t.title = format!("Track number {i}");
                t
            })
            .collect();
        let list = ExportPlaylist { name: "All".into(), track_ids: (1..=2000).collect() };
        let (_, tables) = read(&build_export_pdb(&many, &[list]));
        assert_eq!(rows_of(&tables, PageType::Tracks).len(), 2000);
        assert_eq!(rows_of(&tables, PageType::PlaylistEntries).len(), 2000);
    }

    /// Dev check: with `DUBPLATE_REFERENCE_DEMO_PDB` pointing at the demo export that ships with
    /// rekordcrate, a track row written by rekordbox and ours agree byte for byte, apart from the label
    /// (we write none) and the hash-like field at 0x14.
    #[test]
    fn a_track_row_matches_one_from_rekordbox() {
        let Some(path) = std::env::var_os("DUBPLATE_REFERENCE_DEMO_PDB") else { return };
        let file = std::fs::read(path).expect("reference file");
        // Page 2, row 4: "Demo Track 1".
        let page = &file[2 * PAGE..3 * PAGE];
        let footer = PAGE - 4 - 2 * 5;
        let at = HEAP + u16::from_le_bytes([page[footer], page[footer + 1]]) as usize;
        let next = HEAP + u16::from_le_bytes([page[footer - 2], page[footer - 1]]) as usize;
        let real = &page[at..next];

        let t = ExportTrack {
            id: 5,
            title: "Demo Track 1".into(),
            artist: "Loopmasters".into(),
            album: "".into(),
            genre: "".into(),
            key: None,
            bpm: 128.0,
            duration_secs: 172,
            file_size: 6_899_624,
            sample_rate: 44100,
            bitrate_kbps: 320,
            bit_depth: 16,
            year: 0,
            track_number: 0,
            comment: "Tracks by www.loopmasters.com".into(),
            date_added: "2018-05-25".into(),
            file_path: "/Contents/Loopmasters/UnknownAlbum/Demo Track 1.mp3".into(),
            analyze_path: "/PIONEER/USBANLZ/P016/0000875E/ANLZ0000.DAT".into(),
            analyze_date: "2022-02-02".into(),
        };
        let mut ours = track_row(&t, &Refs { artist: 1, album: 0, genre: 0, key: 5 });
        ours[2..4].copy_from_slice(&0x80u16.to_le_bytes()); // index_shift of the fifth row
                                                            // Same fields, so the same bytes except where we know we differ.
        let skip = |i: usize| (0x14..0x18).contains(&i) || (0x28..0x2c).contains(&i);
        let fixed = 0x88;
        let diffs: Vec<String> = (0..fixed)
            .filter(|&i| !skip(i) && ours[i] != real[i])
            .map(|i| format!("{i:#x}: ours {:#04x}, rekordbox {:#04x}", ours[i], real[i]))
            .collect();
        assert!(diffs.is_empty(), "fixed part differs: {diffs:?}");
        // The strings: compare how the real row lays its first strings out.
        let real_offsets: Vec<u16> =
            (0..21).map(|k| u16::from_le_bytes([real[0x5e + 2 * k], real[0x5f + 2 * k]])).collect();
        let our_offsets: Vec<u16> =
            (0..21).map(|k| u16::from_le_bytes([ours[0x5e + 2 * k], ours[0x5f + 2 * k]])).collect();
        assert_eq!(&our_offsets[..15], &real_offsets[..15], "offsets up to the analysis path");
    }

    /// Dev check: with `DUBPLATE_REFERENCE_PDB` pointing at an *empty* export made by rekordbox, ours
    /// matches it everywhere that is not a sequence number.
    #[test]
    fn matches_rekordbox_for_an_empty_export() {
        let Some(path) = std::env::var_os("DUBPLATE_REFERENCE_PDB") else { return };
        let reference = std::fs::read(path).expect("reference file");
        let ours = build_export_pdb(&[], &[]);
        assert_eq!(ours.len(), reference.len(), "page count");
        // Header page: identical but for the global sequence number.
        let mut head = ours[..PAGE].to_vec();
        head[20..24].copy_from_slice(&reference[20..24]);
        assert_eq!(head, reference[..PAGE], "header page");
        for (i, (a, b)) in ours.chunks(PAGE).zip(reference.chunks(PAGE)).enumerate() {
            if i == 0 {
                continue;
            } // header page: only the sequence number differs, checked below
            let diffs: Vec<String> = a
                .iter()
                .zip(b)
                .enumerate()
                .filter(|(_, (x, y))| x != y)
                .map(|(at, (x, y))| format!("{at:#x}: ours {x:#04x}, rekordbox {y:#04x}"))
                .take(8)
                .collect();
            assert!(diffs.is_empty(), "page {i} differs: {diffs:?}");
        }
    }
}
