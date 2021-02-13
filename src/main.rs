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

// TODO
// HashMap for lines
// HTML output

#[derive(Hash, Clone, Debug)]
struct SnippetDiff {
    path: String,
    old: String,
    new: String,
}

#[derive(Hash, Debug)]
struct LinesRange {
    start_line: usize,
    end_line: usize,
}

#[derive(Debug)]
struct SnippetData {
    diff: SnippetDiff,
    lines: LinesRange,
}

struct CodeSnippets {
    source_filename: String,
    global_metrics: Vec<SnippetDiff>,
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
        // Do not consider spaces parsed ONLY by the new version of
        // a grammar. Since they were not present in an old version, they COULD
        //  be an improvement.
        // FIXME: Find a more decent way to do this
        let without_missing_spaces: Vec<&str> = json_diff
            .lines()
            .filter(|line| !(line.contains("is missing from") || line.is_empty()))
            .collect();

        // Get json diffs information
        let spaces_diff: Vec<SnippetDiff> = without_missing_spaces
            .chunks(5)
            // Do not consider start_line, end_line, space_name, space_kind changes
            .filter(|chunk| {
                !(chunk[0].contains("start_line")
                    || chunk[0].contains("end_line")
                    || chunk[0].contains("name")
                    || chunk[0].contains("kind"))
            })
            .map(|chunk| {
                let path_tmp: Vec<&str> = chunk[0].splitn(3, '"').collect();
                SnippetDiff {
                    path: path_tmp[1].to_owned(),
                    old: chunk[2].trim_start().to_owned(),
                    new: chunk[4].trim_start().to_owned(),
                }
            })
            .collect();

        let mut global_metrics: Vec<SnippetDiff> = Vec::new();
        let mut snippets_data: HashSet<SnippetData> = HashSet::new();

        // Detect spaces path
        let re = Regex::new(r"(spaces\[\d+\])").unwrap();
        for diff in spaces_diff {
            let space_path_items: Vec<String> = re
                .find_iter(&diff.path)
                .map(|mat| {
                    let space_path_item = diff.path.get(mat.start()..mat.end()).unwrap();
                    space_path_item.replace("[", " ").replace("]", "")
                })
                .collect();
            let space_path = space_path_items.join(" ");

            // If empty, it is a global metric
            if space_path.is_empty() {
                global_metrics.push(diff);
            } else {
                let mut value = json2.get("spaces").unwrap();
                for key in space_path.split(' ').skip(1) {
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
                    diff,
                    lines: LinesRange {
                        start_line,
                        end_line,
                    },
                });
            }
        }

        let source_filename = json2.get("name").unwrap().as_str().unwrap().to_owned();
        println!("{}", source_filename);

        Some(CodeSnippets {
            source_filename,
            global_metrics,
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
    snippets: &CodeSnippets,
) -> std::io::Result<()> {
    if !snippets.global_metrics.is_empty() {
        // Print global metrics
        writeln!(writer, "Global Metrics")?;
        for SnippetDiff { path, old, new } in &snippets.global_metrics {
            writeln!(writer, "\npath: {}\nold: {}\nnew: {}\n", path, old, new)?;
        }
    }
    if !snippets.snippets_data.is_empty() {
        // Print snippets data
        writeln!(writer, "Snippets Data")?;
        for SnippetData { diff, lines } in &snippets.snippets_data {
            let str_lines: Vec<&str> = source_file
                .lines()
                .skip(lines.start_line)
                .take(lines.end_line - lines.start_line)
                .collect();
            writeln!(
                writer,
                "\npath: {}\nold: {}\nnew: {}\n",
                diff.path, diff.old, diff.new
            )?;
            writeln!(
                writer,
                "Minimal test - lines ({}, {})",
                lines.start_line + 1,
                lines.end_line
            )?;
            writeln!(writer, "{}\n", str_lines.join("\n"))?;
        }
    }
    Ok(())
}

fn act_on_file(
    path1: &PathBuf,
    path2: &PathBuf,
    output_path: &Option<PathBuf>,
) -> std::io::Result<()> {
    if let Some(snippets) = get_code_snippets(path1, path2) {
        let source_path = PathBuf::from(&snippets.source_filename);
        let source_file = std::fs::read_to_string(&source_path)?;

        let output_filename = get_output_filename(&source_path);
        if let Some(output_path) = output_path {
            let mut output_file = File::create(output_path.join(output_filename))?;
            write(&mut output_file, &source_file, &snippets)?;
        } else {
            let stdout = std::io::stdout();
            let mut stdout = stdout.lock();
            write(&mut stdout, &source_file, &snippets)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_duplicates() {
        let snippets_data: HashSet<SnippetData> = vec![
            SnippetData {
                diff: SnippetDiff {
                    path: "Path1".to_owned(),
                    old: "1.0".to_owned(),
                    new: "2,0".to_owned(),
                },
                lines: LinesRange {
                    start_line: 0,
                    end_line: 42,
                },
            },
            SnippetData {
                diff: SnippetDiff {
                    path: "Path2".to_owned(),
                    old: "1.0".to_owned(),
                    new: "2,0".to_owned(),
                },
                lines: LinesRange {
                    start_line: 0,
                    end_line: 42,
                },
            },
            SnippetData {
                diff: SnippetDiff {
                    path: "Path2".to_owned(),
                    old: "1.0".to_owned(),
                    new: "2,0".to_owned(),
                },
                lines: LinesRange {
                    start_line: 142,
                    end_line: 242,
                },
            },
        ]
        .into_iter()
        .collect();

        let correct_snippets: HashSet<SnippetData> = vec![
            SnippetData {
                diff: SnippetDiff {
                    path: "Path1".to_owned(),
                    old: "1.0".to_owned(),
                    new: "2,0".to_owned(),
                },
                lines: LinesRange {
                    start_line: 0,
                    end_line: 42,
                },
            },
            SnippetData {
                diff: SnippetDiff {
                    path: "Path3".to_owned(),
                    old: "1.0".to_owned(),
                    new: "2,0".to_owned(),
                },
                lines: LinesRange {
                    start_line: 142,
                    end_line: 242,
                },
            },
        ]
        .into_iter()
        .collect();

        assert_eq!(snippets_data, correct_snippets);
    }
}
