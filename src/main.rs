#[macro_use]
extern crate clap;

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::process;

use assert_json_diff::assert_json_eq_no_panic;
use clap::{App, Arg};
use rayon::prelude::*;
use regex::Regex;
use rust_hawktracer::*;
use serde_json::Value;
use walkdir::{DirEntry, WalkDir};

struct Line {
    start_line: usize,
    end_line: usize,
}

struct CodeSnippets {
    filename: String,
    lines: Vec<Line>,
}

#[hawktracer(get_code_snippets)]
fn get_code_snippets(path1: &PathBuf, path2: &PathBuf) -> Option<CodeSnippets> {
    let buffer1 = std::fs::read(path1).unwrap();
    let json1: Value = serde_json::from_slice(&buffer1).unwrap();
    let buffer2 = std::fs::read(path2).unwrap();
    let json2: Value = serde_json::from_slice(&buffer2).unwrap();

    if let Err(diff) = assert_json_eq_no_panic(&json1, &json2) {
        // Detect spaces path
        let re = Regex::new(r"(spaces\[\d+\])").unwrap();
        let only_spaces: Vec<String> = diff
            .lines()
            .map(|line| {
                let all_caps: Vec<String> = re
                    .find_iter(line)
                    .map(|mat| {
                        let caps_str = line.get(mat.start()..mat.end()).unwrap();
                        caps_str.replace("[", " ").replace("]", "")
                    })
                    .collect();
                all_caps.join(" ")
            })
            .filter(|s| !s.is_empty())
            .collect();

        // Get space start and end lines
        let mut lines: Vec<Line> = Vec::new();
        for space in only_spaces {
            let mut value = json1.get("spaces").unwrap();
            for key in space.split(' ').skip(1) {
                value = if let Ok(number) = key.parse::<usize>() {
                    &value.get(number).unwrap()
                } else {
                    &value.get(key).unwrap()
                };
            }
            // Subtracting one because file lines starts from 0
            let start_line = value.get("start_line").unwrap().as_u64().unwrap() - 1;
            let end_line = value.get("end_line").unwrap().as_u64().unwrap();
            lines.push(Line {
                start_line: start_line as usize,
                end_line: end_line as usize,
            });
        }

        let filename = json1.get("name").unwrap().as_str().unwrap().to_owned();
        println!("{}", filename);

        Some(CodeSnippets { filename, lines })
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
    clean_filename.join("_")
}

#[hawktracer(act_on_file)]
fn act_on_file(
    path1: &PathBuf,
    path2: &PathBuf,
    output_path: &Option<PathBuf>,
) -> std::io::Result<()> {
    if let Some(snippet) = get_code_snippets(path1, path2) {
        let source_path = PathBuf::from(snippet.filename);
        let source_file = std::fs::read_to_string(&source_path)?;

        let output_filename = get_output_filename(&source_path);
        if let Some(output_path) = output_path {
            let mut output_file = File::create(output_path.join(output_filename + ".txt"))?;
            write(&mut output_file, &source_file, &snippet.lines)?;
        } else {
            let stdout = std::io::stdout();
            let mut stdout = stdout.lock();
            write(&mut stdout, &source_file, &snippet.lines)?;
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

#[hawktracer(explore)]
fn explore(
    path1: &PathBuf,
    path2: &PathBuf,
    output_path: &Option<PathBuf>,
    limit_files: usize,
) -> std::io::Result<()> {
    // Get (PathBuf, PathBuf) in a parallel way
    let all_files: Vec<_> = WalkDir::new(&path1)
        .into_iter()
        .filter_entry(|e| !is_hidden(e))
        .zip(
            WalkDir::new(&path2)
                .into_iter()
                .filter_entry(|e| !is_hidden(e)),
        )
        .par_bridge()
        .filter(|(entry1, entry2)| {
            entry1.as_ref().unwrap().path().is_file() && entry2.as_ref().unwrap().path().is_file()
        })
        .map(|(entry1, entry2)| {
            let entry1 = entry1.as_ref().unwrap();
            let path1_file: PathBuf = entry1.path().to_path_buf();
            let entry2 = entry2.as_ref().unwrap();
            let path2_file: PathBuf = entry2.path().to_path_buf();
            (path1_file, path2_file)
        })
        .collect();

    let chunks: Vec<Vec<(PathBuf, PathBuf)>> = all_files.into_par_iter().chunks(5000).collect();

    let _: Vec<_> = chunks
        .iter()
        .par_bridge()
        .map(|v| {
            for (path1, path2) in v {
                act_on_file(&path1, &path2, &output_path).unwrap();
            }
        })
        .collect();

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

fn write<W: Write>(writer: &mut W, source_file: &str, lines: &[Line]) -> std::io::Result<()> {
    for Line {
        start_line,
        end_line,
    } in lines
    {
        let lines: Vec<&str> = source_file
            .lines()
            .skip(*start_line)
            .take(*end_line - *start_line)
            .collect();
        writeln!(writer, "Minimal test - line ({}, {})", start_line, end_line)?;
        writeln!(writer, "{}\n", lines.join("\n"))?;
    }
    Ok(())
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
                .help("Output file/directory")
                .short("o")
                .long("output")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("limit-files")
                .help("Limit number of files")
                .short("l")
                .long("limit-files")
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
    let limit_files = if let Some(num_files) = matches.value_of("limit-files") {
        num_files
            .parse::<usize>()
            .map_or(std::usize::MAX, |v| v + 1)
    } else {
        std::usize::MAX
    };

    exist_or_exit(&path1, "first");
    exist_or_exit(&path2, "second");

    let instance = HawktracerInstance::new();
    let _listener = instance.create_listener(HawktracerListenerType::ToFile {
        file_path: "trace.bin".into(),
        buffer_size: 4096,
    });

    if path1.is_dir() && path2.is_dir() {
        explore(&path1, &path2, &output_path, limit_files).unwrap();
    } else if (path1.is_dir() && !path2.is_dir()) || (!path1.is_dir() && path2.is_dir()) {
        eprintln!("Both the paths should be a directory or a file",);
        process::exit(1);
    } else {
        act_on_file(&path1, &path2, &output_path).unwrap();
    }
}
