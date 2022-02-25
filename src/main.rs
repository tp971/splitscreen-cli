use std::env;
use std::error::Error;
use std::io;
use std::path::Path;
use std::process;

use clap::{AppSettings, Arg, ArgGroup, Command, crate_version};

mod splitscreen;
use splitscreen::{Config, Compare, Encoder, Input};

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {}", err);
        process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let matches = Command::new("splitscreen-cli")
        .version(crate_version!())
        .setting(AppSettings::DeriveDisplayOrder)

        .arg(Arg::new("resolution")
            .long("res")
            .short('s')
            .required(true)
            .value_name("WIDTHxHEIGHT")
            .help("Set resolution to WIDTHxHEIGHT"))

        .arg(Arg::new("fps")
            .long("fps")
            .short('r')
            .required(true)
            .value_name("FPS")
            .help("Set frame rate to FPS"))

        .group(ArgGroup::new("cmp-type")
            .args(&["cmp-loss", "cmp-save"]))
        .arg(Arg::new("cmp-loss")
            .long("cmp-loss")
            .help("Compare time loss"))
        .arg(Arg::new("cmp-save")
            .long("cmp-save")
            .help("Compare time save"))

        .arg(Arg::new("pause")
            .long("pause")
            .short('p')
            .value_name("SECONDS")
            .help("Pause for SECONDS seconds after each split"))

        .arg(Arg::new("output")
            .long("out")
            .short('o')
            .value_name("FILENAME")
            .help("Render video into FILENAME"))

        .arg(Arg::new("encoder")
            .long("encoder")
            .short('e')
            .value_name("ENCODER")
            .help("Use ENCODER for video encoding (one of x264 (default), vaapi, nvenc, amf, qsv)"))

        .arg(Arg::new("raw")
            .long("raw")
            .help("Output rawvideo"))

        .arg(Arg::new("report")
            .long("report")
            .help("Report progress to stderr"))

        .group(ArgGroup::new("input-type")
            .args(&["input-files", "input-args"]))
        .arg(Arg::new("input-files")
            .long("input-files")
            .short('F')
            .help("Interpret INPUT as pairs of video and split files, i.e. INPUT = video1 splitfile1 video2 splifile2 ... (this is the default behavior)"))
        .arg(Arg::new("input-args")
            .long("input-args")
            .short('A')
            .help("Interpret INPUT as video files combined with arguments, seperated by `--`, i.e. INPUT = video1 arg arg ... -- video2 arg arg ... -- ..."))

        .arg(Arg::new("input")
            .index(1)
            .multiple_occurrences(true)
            .required(true)
            .value_name("INPUT")
            .help("Input (see above)"))

        .get_matches();

    let encoders = Encoder::all();

    let res = matches.value_of("resolution").unwrap();
    let res_split: Vec<_> = res.split("x").collect();
    if res_split.len() != 2 {
        Err(format!("invalid resolution: {}", res))?;
    }
    let width = res_split[0].parse()
        .map_err(|_| format!("invalid resolution: {}", res))?;
    let height = res_split[1].parse()
        .map_err(|_| format!("invalid resolution: {}", res))?;

    let fps_str = matches.value_of("fps").unwrap();
    let fps = fps_str.parse()
        .map_err(|_| format!("invalid frame rate: {}", fps_str))?;

    let cmp =
        if matches.is_present("cmp-loss") {
            Some(Compare::TimeLoss)
        } else if matches.is_present("cmp-save") {
            Some(Compare::TimeSave)
        } else {
            None
        };

    let pause =
        if let Some(s) = matches.value_of("pause") {
            s.parse().map_err(|_| format!("invalid number: {}", s))?
        } else {
            0.0
        };

    let output = matches.value_of("output");

    let encoder =
        if let Some(val) = matches.value_of("encoder") {
            *encoders.iter()
                .find(|e| e.to_string() == val)
                .ok_or_else(|| format!("unknown encoder: {}", val))?
        } else {
            Encoder::X264
        };

    let raw = matches.is_present("raw");

    let report = matches.is_present("report");



    let mut inputs = Vec::new();
    if matches.is_present("input-args") {
        let mut it = matches.values_of("input").unwrap();
        while let Some(video_path) = it.next() {
            if video_path == "--" {
                continue;
            }
            let video_path = Path::new(video_path);
            let args = it.by_ref().take_while(|s| *s != "--");
            let input = Input::from_args(video_path, args.into_iter())?;
            inputs.push(input);
        }
    } else {
        let mut it = matches.values_of("input").unwrap();
        while let Some(video_path) = it.next() {
            let video_path = Path::new(video_path);
            if let Some(split_file) = it.next() {
                let input = Input::from_file(video_path, Path::new(split_file))?;
                inputs.push(input);
            } else {
                Err(format!("error: missing split file for video: {:?}", video_path))?;
            }
        }
    }



    let config = Config {
        width, height, fps, cmp, pause, inputs
    };

    eprintln!("{:#?}", config);

    let info = config.prepare()?;

    eprintln!("{:#?}", info);
    
    if let Some(name) = output {
        if raw {
            if name == "-" {
                config.render_raw(&info, io::stdout(), report)?;
            } else {
                config.render_raw_to_file(&info, Path::new(name), report)?;
            }
        } else {
            if name == "-" {
                config.encode_to_stdout(&info, encoder, report)?;
            } else {
                config.encode_to_file(&info, encoder, report, Path::new(name))?;
            }
        }

    } else {
        config.play(&info)?;
    }

    Ok(())
}
