#[macro_use]
extern crate clap;

mod non_utf8;

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::{process, thread};

use assert_json_diff::{assert_json_matches_no_panic, CompareMode, Config};
use clap::{App, Arg};
use crossbeam::channel::{unbounded, Receiver, Sender};
use regex::Regex;
use serde_json::Value;
use walkdir::{DirEntry, WalkDir};

use non_utf8::{encode_to_utf8, read_file_with_eol};

#[derive(Clone, Debug)]
struct SnippetDiff {
    path: String,
    old: String,
    new: String,
}

#[derive(Hash, Eq, PartialEq, Debug)]
struct LinesRange {
    start_line: usize,
    end_line: usize,
}

struct CodeSnippets {
    source_filename: String,
    global_metrics: Vec<SnippetDiff>,
    snippets_data: HashMap<LinesRange, Vec<SnippetDiff>>,
}

struct JobItem {
    path1: PathBuf,
    path2: PathBuf,
    output_path: Option<PathBuf>,
}

type JobReceiver = Receiver<Option<JobItem>>;
type JobSender = Sender<Option<JobItem>>;

fn get_code_snippets(path1: &Path, path2: &Path) -> Option<CodeSnippets> {
    let buffer1 = match std::fs::read(path1) {
        Ok(buffer1) => buffer1,
        Err(_) => return None,
    };
    let json1: Value = match serde_json::from_slice(&buffer1) {
        Ok(json1) => json1,
        Err(_) => return None,
    };
    let buffer2 = match std::fs::read(path2) {
        Ok(buffer2) => buffer2,
        Err(_) => return None,
    };
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
                    || chunk[0].contains("kind")
                    || chunk[0].contains("halstead.length")
                    || chunk[0].contains("halstead.volume")
                    || chunk[0].contains("halstead.vocabulary")
                    || chunk[0].contains("halstead.purity_ratio")
                    || chunk[0].contains("halstead.level")
                    || chunk[0].contains("halstead.estimated_program_length")
                    || chunk[0].contains("halstead.time")
                    || chunk[0].contains("halstead.bugs")
                    || chunk[0].contains("halstead.difficulty")
                    || chunk[0].contains("halstead.effort")
                    || chunk[0].contains("metrics.mi")
                    || chunk[0].contains("average"))
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
        let mut snippets_data: HashMap<LinesRange, Vec<SnippetDiff>> = HashMap::new();

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
                        value.get(number).unwrap()
                    } else {
                        value.get(key).unwrap()
                    };
                }
                // Subtracting one since the lines of a file start from 0
                let start_line = value.get("start_line").unwrap().as_u64().unwrap() as usize - 1;
                let end_line = value.get("end_line").unwrap().as_u64().unwrap() as usize;
                let lines_range = LinesRange {
                    start_line,
                    end_line,
                };
                if let Some(val) = snippets_data.get_mut(&lines_range) {
                    val.push(diff);
                } else {
                    snippets_data.insert(lines_range, vec![diff]);
                }
            }
        }

        let source_filename = json2.get("name").unwrap().as_str().unwrap().to_owned();
        println!("{source_filename}");

        Some(CodeSnippets {
            source_filename,
            global_metrics,
            snippets_data,
        })
    } else {
        None
    }
}

fn get_output_filename(source_path: &Path) -> String {
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
    clean_filename.join("_") + ".html"
}

fn write<W: Write>(
    writer: &mut W,
    output_filename: &str,
    source_file: &str,
    snippets: &CodeSnippets,
) -> std::io::Result<()> {
    writeln!(
        writer,
        "<!DOCTYPE html>
<html>
<head>
    <title>{}</title>
</head>
<body>",
        output_filename
    )?;
    if !snippets.global_metrics.is_empty() {
        // Print global metrics
        writeln!(writer, "<h1>Global Metrics</h1>")?;
        for SnippetDiff { path, old, new } in &snippets.global_metrics {
            writeln!(
                writer,
                "<b>path:</b> {} <br>
<b>old:</b> {} <br>
<b>new:</b> {} <br><br>",
                path, old, new
            )?;
        }
    }
    if !snippets.global_metrics.is_empty() && snippets.snippets_data.is_empty() {
        writeln!(writer, "<h2>Code</h2>")?;
        writeln!(writer, "<pre><i>{}</i></pre>\n", source_file)?;
    }
    if !snippets.snippets_data.is_empty() {
        // Print spaces data
        writeln!(writer, "<h1>Spaces Data</h1>")?;
        for (lines_range, diffs) in &snippets.snippets_data {
            writeln!(
                writer,
                "<h2>Minimal test - lines ({}, {})</h2>",
                lines_range.start_line + 1,
                lines_range.end_line
            )?;
            for diff in diffs {
                writeln!(
                    writer,
                    "<b>path:</b> {}<br>
<b>old:</b> {}<br>
<b>new:</b> {}<br><br>",
                    diff.path, diff.old, diff.new
                )?;
            }
            writeln!(writer, "<h3>Code</h3>")?;
            let str_lines: Vec<&str> = source_file
                .lines()
                .skip(lines_range.start_line)
                .take(lines_range.end_line - lines_range.start_line)
                .collect();
            writeln!(writer, "<pre><i>{}</i></pre>\n", str_lines.join("\n"))?;
        }
    }
    writeln!(
        writer,
        "</body>
</html>"
    )?;
    Ok(())
}

fn act_on_file(
    path1: PathBuf,
    path2: PathBuf,
    output_path: Option<PathBuf>,
) -> std::io::Result<()> {
    if let Some(snippets) = get_code_snippets(&path1, &path2) {
        let source_path = PathBuf::from(&snippets.source_filename);
        let source_file_bytes = match read_file_with_eol(&source_path) {
            Ok(source_file_bytes) => match source_file_bytes {
                Some(bytes) => bytes,
                None => return Ok(()),
            },
            Err(_) => return Ok(()),
        };

        let source_file = match std::str::from_utf8(&source_file_bytes) {
            Ok(source_file) => source_file.to_owned(),
            Err(_) => match encode_to_utf8(&source_file_bytes) {
                Ok(source_file) => source_file,
                Err(_) => return Ok(()),
            },
        };

        let source_escape_html = html_escape::encode_text(&source_file);

        let output_filename = get_output_filename(&source_path);
        if let Some(output_path) = output_path {
            let mut output_file = File::create(output_path.join(&output_filename))?;
            write(
                &mut output_file,
                &output_filename,
                &source_escape_html,
                &snippets,
            )?;
        } else {
            let stdout = std::io::stdout();
            let mut stdout = stdout.lock();
            write(
                &mut stdout,
                &output_filename,
                &source_escape_html,
                &snippets,
            )?;
        }
    }

    Ok(())
}

fn consumer(receiver: JobReceiver) {
    while let Ok(job) = receiver.recv() {
        if job.is_none() {
            break;
        }
        let job = job.unwrap();
        let path1 = job.path1.clone();
        let path2 = job.path2.clone();

        if let Err(err) = act_on_file(job.path1, job.path2, job.output_path) {
            eprintln!("{:?} for files {:?} {:?}", err, path1, path2);
        }
    }
}

fn send_file(path1: PathBuf, path2: PathBuf, output_path: Option<PathBuf>, sender: &JobSender) {
    sender
        .send(Some(JobItem {
            path1,
            path2,
            output_path,
        }))
        .unwrap();
}

fn is_hidden(entry: &DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| s.starts_with('.'))
        .unwrap_or(false)
}

fn explore(path1: PathBuf, path2: PathBuf, output_path: Option<PathBuf>, sender: &JobSender) {
    if path1.is_dir() && path2.is_dir() {
        WalkDir::new(&path1)
            .into_iter()
            .filter_entry(|e| !is_hidden(e))
            .zip(
                WalkDir::new(&path2)
                    .into_iter()
                    .filter_entry(|e| !is_hidden(e)),
            )
            .for_each(|(entry1, entry2)| {
                let entry1 = entry1.as_ref().unwrap();
                let path1_file: PathBuf = entry1.path().to_path_buf();
                let entry2 = entry2.as_ref().unwrap();
                let path2_file: PathBuf = entry2.path().to_path_buf();
                if path1_file.is_file()
                    && path2_file.is_file()
                    && path1_file.extension().unwrap() == "json"
                    && path2_file.extension().unwrap() == "json"
                    && path1_file.file_name().unwrap() == path2_file.file_name().unwrap()
                {
                    send_file(path1_file, path2_file, output_path.clone(), sender);
                }
            });
    } else {
        send_file(path1, path2, output_path, sender);
    }
}

#[inline(always)]
fn exist_or_exit(path: &Path, which_path: &str) {
    if !(path.exists()) {
        eprintln!(
            "The {which_path} path `{}` is not correct",
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

    if (path1.is_dir() && !path2.is_dir()) || (!path1.is_dir() && path2.is_dir()) {
        eprintln!("Both the paths should be a directory or a file",);
        process::exit(1);
    }

    let num_jobs = std::cmp::max(2, num_cpus::get()) - 1;

    let (sender, receiver) = unbounded();

    let producer = {
        let sender = sender.clone();

        thread::Builder::new()
            .name(String::from("Producer"))
            .spawn(move || explore(path1, path2, output_path, &sender))
            .unwrap()
    };

    let mut receivers = Vec::with_capacity(num_jobs);
    for i in 0..num_jobs {
        let receiver = receiver.clone();

        let thread = thread::Builder::new()
            .name(format!("Consumer {}", i))
            .spawn(move || {
                consumer(receiver);
            })
            .unwrap();

        receivers.push(thread);
    }

    if producer.join().is_err() {
        process::exit(1);
    }

    // Poison the receiver, now that the producer is finished.
    for _ in 0..num_jobs {
        sender.send(None).unwrap();
    }

    for receiver in receivers {
        if receiver.join().is_err() {
            process::exit(1);
        }
    }
}
