#[macro_use]
extern crate clap;

use std::collections::HashSet;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::PathBuf;
use std::process;

use assert_json_diff::{assert_json_matches_no_panic, CompareMode, Config};
use clap::{App, Arg};
use rayon::prelude::*;
use regex::Regex;
use serde_json::Value;
use walkdir::{DirEntry, WalkDir};

struct SpaceDiff<'a> {
    diff: &'a str,
    metric_diff_path: String,
}

#[derive(Hash, Debug)]
struct LinesRange {
    start_line: usize,
    end_line: usize,
}

#[derive(Debug)]
struct SnippetData {
    diff: String,
    lines: LinesRange,
}

struct CodeSnippets {
    source_filename: String,
    snippets_data: HashSet<SnippetData>,
}

impl PartialEq for SnippetData {
    fn eq(&self, other: &Self) -> bool {
        self.lines.start_line == other.lines.start_line
            && self.lines.end_line == other.lines.end_line
    }
}

impl Eq for SnippetData {}

impl Hash for SnippetData {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.lines.start_line.hash(state);
        self.lines.end_line.hash(state);
    }
}

fn get_code_snippets(path1: &PathBuf, path2: &PathBuf) -> Option<CodeSnippets> {
    let buffer1 = std::fs::read(path1).unwrap();
    let json1: Value = match serde_json::from_slice(&buffer1) {
        Ok(json1) => json1,
        Err(_) => return None,
    };
    let buffer2 = std::fs::read(path2).unwrap();
    let json2: Value = match serde_json::from_slice(&buffer2) {
        Ok(json2) => json2,
        Err(_) => return None,
    };

    // Two JSON values MUST be exactly equal
    let config = Config::new(CompareMode::Strict);

    if let Err(json_diff) = assert_json_matches_no_panic(&json1, &json2, config) {
        // Detect spaces path
        let re = Regex::new(r"(spaces\[\d+\])").unwrap();
        let only_spaces: Vec<SpaceDiff> = json_diff
            .lines()
            .map(|line| {
                let all_caps: Vec<String> = re
                    .find_iter(line)
                    .map(|mat| {
                        let caps_str = line.get(mat.start()..mat.end()).unwrap();
                        caps_str.replace("[", " ").replace("]", "")
                    })
                    .collect();
                SpaceDiff {
                    diff: line,
                    metric_diff_path: all_caps.join(" "),
                }
            })
            .filter(|s| !s.metric_diff_path.is_empty())
            .collect();

        let mut snippets_data: HashSet<SnippetData> = HashSet::new();

        // If there are no spaces differences, but only global ones, that means
        // there are no spaces at all in the source file. So the entire
        // source code is considered.
        if only_spaces.is_empty() {
            // Subtracting one since the lines of a file start from 0
            let start_line = json1.get("start_line").unwrap().as_u64().unwrap() as usize - 1;
            let end_line = json1.get("end_line").unwrap().as_u64().unwrap() as usize;
            snippets_data.insert(SnippetData {
                diff: json_diff,
                lines: LinesRange {
                    start_line,
                    end_line,
                },
            });
        } else {
            // Get space start and end lines
            for space in only_spaces {
                let mut value = json1.get("spaces").unwrap();
                for key in space.metric_diff_path.split(' ').skip(1) {
                    value = if let Ok(number) = key.parse::<usize>() {
                        &value.get(number).unwrap()
                    } else {
                        &value.get(key).unwrap()
                    };
                }
                // Subtracting one since the lines of a file start from 0
                let start_line = value.get("start_line").unwrap().as_u64().unwrap() as usize - 1;
                let end_line = value.get("end_line").unwrap().as_u64().unwrap() as usize;
                snippets_data.insert(SnippetData {
                    diff: space.diff.to_owned(),
                    lines: LinesRange {
                        start_line,
                        end_line,
                    },
                });
            }
        }

        let source_filename = json1.get("name").unwrap().as_str().unwrap().to_owned();
        println!("{}", source_filename);

        Some(CodeSnippets {
            source_filename,
            snippets_data,
        })
    } else {
        None
    }
}

fn get_output_filename(source_path: &PathBuf) -> String {
    let clean_filename: Vec<&str> = source_path
        .iter()
        .filter(|v| {
            if let Some(s) = v.to_str() {
                ![".", "..", ":", "/", "\\"].contains(&s)
            } else {
                false
            }
        })
        .map(|s| s.to_str().unwrap())
        .collect();
    clean_filename.join("_") + ".txt"
}

fn write<W: Write>(
    writer: &mut W,
    source_file: &str,
    snippets_data: &HashSet<SnippetData>,
) -> std::io::Result<()> {
    for SnippetData { diff, lines } in snippets_data {
        let str_lines: Vec<&str> = source_file
            .lines()
            .skip(lines.start_line)
            .take(lines.end_line - lines.start_line)
            .collect();
        writeln!(writer, "Diff: {}", diff,)?;
        writeln!(
            writer,
            "Minimal test - lines ({}, {})",
            lines.start_line.max(1),
            lines.end_line
        )?;
        writeln!(writer, "{}\n", str_lines.join("\n"))?;
    }
    Ok(())
}

fn act_on_file(
    path1: &PathBuf,
    path2: &PathBuf,
    output_path: &Option<PathBuf>,
) -> std::io::Result<()> {
    if let Some(snippets) = get_code_snippets(path1, path2) {
        let source_path = PathBuf::from(snippets.source_filename);
        let source_file = std::fs::read_to_string(&source_path)?;

        let output_filename = get_output_filename(&source_path);
        if let Some(output_path) = output_path {
            let mut output_file = File::create(output_path.join(output_filename))?;
            write(&mut output_file, &source_file, &snippets.snippets_data)?;
        } else {
            let stdout = std::io::stdout();
            let mut stdout = stdout.lock();
            write(&mut stdout, &source_file, &snippets.snippets_data)?;
        }
    }

    Ok(())
}

fn is_hidden(entry: &DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| s.starts_with('.'))
        .unwrap_or(false)
}

fn explore(path1: &PathBuf, path2: &PathBuf, output_path: &Option<PathBuf>) -> std::io::Result<()> {
    WalkDir::new(&path1)
        .into_iter()
        .filter_entry(|e| !is_hidden(e))
        .zip(
            WalkDir::new(&path2)
                .into_iter()
                .filter_entry(|e| !is_hidden(e)),
        )
        .par_bridge()
        .for_each(|(entry1, entry2)| {
            let entry1 = entry1.as_ref().unwrap();
            let path1_file: PathBuf = entry1.path().to_path_buf();
            let entry2 = entry2.as_ref().unwrap();
            let path2_file: PathBuf = entry2.path().to_path_buf();
            if path1_file.is_file()
                && path2_file.is_file()
                && path1_file.extension().unwrap() == "json"
                && path2_file.extension().unwrap() == "json"
            {
                act_on_file(&path1_file, &path2_file, &output_path).unwrap();
            }
        });

    Ok(())
}

#[inline(always)]
fn exist_or_exit(path: &PathBuf, which_path: &str) {
    if !(path.exists()) {
        eprintln!(
            "The {} path `{}` is not correct",
            which_path,
            path.to_str().unwrap()
        );
        process::exit(1);
    }
}

fn main() {
    let matches = App::new("json-minimal-tests")
        .version(crate_version!())
        .author(&*env!("CARGO_PKG_AUTHORS").replace(':', "\n"))
        .about(
            "Find the minimal tests from a source code using the differences
between the metrics of the two JSON files passed in input.",
        )
        .arg(
            Arg::with_name("output")
                .help("Output directory")
                .short("o")
                .long("output")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("first-json")
                .help("Old json file")
                .required(true)
                .takes_value(true),
        )
        .arg(
            Arg::with_name("second-json")
                .help("New json file")
                .required(true)
                .takes_value(true),
        )
        .get_matches();

    let path1 = PathBuf::from(matches.value_of("first-json").unwrap());
    let path2 = PathBuf::from(matches.value_of("second-json").unwrap());
    let output_path = if let Some(path) = matches.value_of("output") {
        let path = PathBuf::from(path);
        exist_or_exit(&path, "output");
        Some(path)
    } else {
        None
    };

    exist_or_exit(&path1, "first");
    exist_or_exit(&path2, "second");

    if path1.is_dir() && path2.is_dir() {
        explore(&path1, &path2, &output_path).unwrap();
    } else if (path1.is_dir() && !path2.is_dir()) || (!path1.is_dir() && path2.is_dir()) {
        eprintln!("Both the paths should be a directory or a file",);
        process::exit(1);
    } else {
        act_on_file(&path1, &path2, &output_path).unwrap();
    }
}
