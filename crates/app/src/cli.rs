//! Command-line tools for working with MIDI mappings (no window is opened).
//!
//! ```text
//! dubplate --import FILE [OUT_DIR]                    convert one map (.djm / Mixxx .xml / rekordbox .csv)
//! dubplate --import-dir SOURCE DIR OUT_DIR            convert every map in a folder
//! dubplate --midi-survey SOURCE DIR [OUT.md]          which functions does a program's map set use?
//! dubplate --midi-docs OUT_DIR SOURCE=DIR ...         write the per-program survey reports (keep them out of git)
//! dubplate --midi-compare A.toml B.toml               do two profiles of one device agree?
//! ```
//! SOURCE is `djuced`, `mixxx` or `rekordbox`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::importers::{self, compare, survey, Report, Source};
use crate::midi_profile::{self, Profile};

const USAGE: &str = "usage:
  dubplate --import FILE [OUT_DIR]
  dubplate --import-dir SOURCE DIR OUT_DIR
  dubplate --midi-survey SOURCE DIR [OUT.md]
  dubplate --midi-docs OUT_DIR SOURCE=DIR [SOURCE=DIR ...]
  dubplate --midi-compare A.toml B.toml
SOURCE is djuced, mixxx or rekordbox.";

/// Runs a CLI command and returns the exit code, or None if `args` is not one (start the app).
pub fn run(args: &[String]) -> Option<i32> {
    let cmd = args.first()?.as_str();
    let rest = &args[1..];
    let result = match cmd {
        "--import" | "--import-djm" => import_one(rest),
        "--import-dir" => import_dir(rest),
        "--midi-survey" => midi_survey(rest),
        "--midi-docs" => midi_docs(rest),
        "--midi-compare" => midi_compare(rest),
        "--midi-help" => {
            println!("{USAGE}");
            Ok(())
        }
        _ => return None,
    };
    Some(match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}\n\n{USAGE}");
            1
        }
    })
}

fn source_arg(s: Option<&String>) -> Result<Source, String> {
    s.and_then(|s| Source::from_slug(s)).ok_or_else(|| "SOURCE must be djuced, mixxx or rekordbox".to_owned())
}

fn convert_file(path: &Path) -> Result<(Source, Profile, Report), String> {
    let text = importers::read_text(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let source = importers::detect(path, &text)
        .ok_or_else(|| format!("{}: not a .djm, Mixxx .xml or rekordbox .csv map", path.display()))?;
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("controller");
    let (profile, report) = importers::import(source, &text, stem).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((source, profile, report))
}

fn write_profile(profile: &Profile, out_dir: &Path, id: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    let path = out_dir.join(format!("{id}.toml"));
    std::fs::write(&path, profile.to_toml()).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

fn stem_id(path: &Path) -> String {
    let stem = path.file_name().and_then(|s| s.to_str()).unwrap_or("controller");
    let stem = stem.split('.').next().unwrap_or(stem);
    midi_profile::slug(stem)
}

fn print_report(report: &Report) {
    for a in &report.approximations {
        println!("  approx  {a}");
    }
    for (n, why) in &report.skipped {
        println!("  skipped {n}: {why}");
    }
}

fn import_one(args: &[String]) -> Result<(), String> {
    let file = args.first().ok_or("missing FILE")?;
    let path = PathBuf::from(file);
    let (source, profile, report) = convert_file(&path)?;
    let out_dir = args.get(1).map(PathBuf::from).unwrap_or_else(midi_profile::user_profile_dir);
    let out = write_profile(&profile, &out_dir, &stem_id(&path))?;
    println!("{} ({}): {} bindings -> {}", profile.device.name, source.name(), report.converted, out.display());
    print_report(&report);
    Ok(())
}

fn import_dir(args: &[String]) -> Result<(), String> {
    let source = source_arg(args.first())?;
    let dir = PathBuf::from(args.get(1).ok_or("missing DIR")?);
    let out_dir = PathBuf::from(args.get(2).ok_or("missing OUT_DIR")?);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.to_lowercase().ends_with(source.suffix())))
        .collect();
    files.sort();

    let mut done: Vec<(PathBuf, Profile, Report)> = Vec::new();
    for f in files {
        match convert_file(&f) {
            Ok((s, p, r)) if s == source => done.push((f, p, r)),
            Ok(_) => eprintln!("{}: wrong kind of map for {}", f.display(), source.name()),
            Err(e) => eprintln!("{e}"),
        }
    }
    // Different maps for one device share a name; tell them apart by file.
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (_, p, _) in &done {
        *counts.entry(p.device.name.clone()).or_default() += 1;
    }
    let mut written = 0;
    for (f, mut p, r) in done {
        if counts[&p.device.name] > 1 {
            let id = stem_id(&f);
            p.device.name = format!("{} [{id}]", p.device.name);
        }
        if r.converted == 0 {
            println!("{}: nothing convertible, not written", p.device.name);
            continue;
        }
        write_profile(&p, &out_dir, &stem_id(&f))?;
        println!("{}: {} bindings, {} left out", p.device.name, r.converted, r.skipped.len());
        written += 1;
    }
    println!("{written} profiles written to {}", out_dir.display());
    Ok(())
}

fn midi_survey(args: &[String]) -> Result<(), String> {
    let source = source_arg(args.first())?;
    let dir = PathBuf::from(args.get(1).ok_or("missing DIR")?);
    let md = survey::render(&survey::survey_dir(source, &dir)?);
    match args.get(2) {
        Some(out) => std::fs::write(out, md).map_err(|e| format!("{out}: {e}"))?,
        None => print!("{md}"),
    }
    Ok(())
}

fn midi_docs(args: &[String]) -> Result<(), String> {
    let out_dir = PathBuf::from(args.first().ok_or("missing OUT_DIR")?);
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    let mut surveys = Vec::new();
    for pair in &args[1..] {
        let (src, dir) = pair.split_once('=').ok_or_else(|| format!("`{pair}`: expected SOURCE=DIR"))?;
        let source = Source::from_slug(src).ok_or_else(|| format!("unknown source `{src}`"))?;
        let s = survey::survey_dir(source, Path::new(dir))?;
        let path = out_dir.join(format!("{}-functions.md", source.slug()));
        std::fs::write(&path, survey::render(&s)).map_err(|e| format!("{}: {e}", path.display()))?;
        println!("{}: {} maps, {} functions -> {}", source.name(), s.devices.len(), s.functions.len(), path.display());
        surveys.push(s);
    }
    let path = out_dir.join("dubplate-functions.md");
    std::fs::write(&path, survey::render_matrix(&surveys)).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("-> {}", path.display());
    Ok(())
}

fn load_any(path: &str) -> Result<Profile, String> {
    let p = Path::new(path);
    if p.extension().and_then(|e| e.to_str()) == Some("toml") {
        let text = std::fs::read_to_string(p).map_err(|e| format!("{path}: {e}"))?;
        let profile = Profile::from_toml(&text).map_err(|e| format!("{path}: {e}"))?;
        profile.compile().map_err(|e| format!("{path}: {e}"))?;
        Ok(profile)
    } else {
        convert_file(p).map(|(_, profile, _)| profile)
    }
}

fn midi_compare(args: &[String]) -> Result<(), String> {
    let (a, b) = (args.first().ok_or("missing A")?, args.get(1).ok_or("missing B")?);
    let (pa, pb) = (load_any(a)?, load_any(b)?);
    let c = compare::compare(&pa, &pb);
    print!(
        "{}",
        compare::render_markdown(
            &format!("{} ({})", pa.device.name, pa.device.source),
            &format!("{} ({})", pb.device.name, pb.device.source),
            &c
        )
    );
    Ok(())
}
