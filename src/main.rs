use std::env;
use std::io;
use std::path::Path;

use clap::{App, AppSettings, Arg, ArgGroup, crate_version};

mod splitscreen;
use splitscreen::{Config, Compare, Input};

fn main() {
    let matches = App::new("splitscreen")
        .version(crate_version!())
        .setting(AppSettings::DeriveDisplayOrder)
        .setting(AppSettings::UnifiedHelpMessage)
        .arg(Arg::with_name("resolution")
            .long("res")
            .short("r")
            .required(true)
            .value_name("WIDTHxHEIGHT")
            .help("Set resolution to WIDTHxHEIGHT"))
        .arg(Arg::with_name("fps")
            .long("fps")
            .short("R")
            .required(true)
            .value_name("FRAMERATE")
            .help("Set fps to FRAMERATE"))
        .arg(Arg::with_name("cut-start")
            .long("cut-start")
            .value_name("SECONDS")
            .help("Start video SECONDS seconds before the first split"))
        .arg(Arg::with_name("cut-end")
            .long("cut-end")
            .value_name("SECONDS")
            .help("End video SECONDS seconds after the last split"))
        .group(ArgGroup::with_name("cmp")
            .args(&["cmp-loss", "cmp-save"]))
        .arg(Arg::with_name("cmp-loss")
            .long("cmp-loss")
            .help("Compare time loss"))
        .arg(Arg::with_name("cmp-save")
            .long("cmp-save")
            .help("Compare time save"))
        .arg(Arg::with_name("pause")
            .long("pause")
            .short("p")
            .value_name("SECONDS")
            .help("Pause for SECONDS seconds after each split"))
        .arg(Arg::with_name("output")
            .long("out")
            .short("o")
            .value_name("FILENAME")
            .help("Render video into FILENAME"))
        .arg(Arg::with_name("raw")
            .long("raw")
            .help("Output rawvideo"))
        .arg(Arg::with_name("report")
            .long("report")
            .help("Report progress to stderr"))
        .arg(Arg::with_name("input")
            .index(1)
            .multiple(true)
            .required(true)
            .value_name("INPUT")
            .help("Input (see above)"))
        .get_matches();

    let res = matches.value_of("resolution").unwrap();
    let res_split: Vec<_> = res.split("x").collect();
    assert!(res_split.len() == 2);
    let width = res_split[0].parse().unwrap();
    let height = res_split[1].parse().unwrap();

    let fps = matches.value_of("fps").unwrap().parse().unwrap();

    let cut_start = matches.value_of("cut-start")
        .map(|s| s.parse().unwrap());

    let cut_end = matches.value_of("cut-end")
        .map(|s| s.parse().unwrap());

    let cmp =
        if matches.is_present("cmp-loss") {
            Some(Compare::TimeLoss)
        } else if matches.is_present("cmp-save") {
            Some(Compare::TimeSave)
        } else {
            None
        };

    let pause = matches.value_of("pause")
        .map_or(0.0, |s| s.parse().unwrap());

    let output = matches.value_of("output");

    let raw = matches.is_present("raw");

    let report = matches.is_present("report");



    let mut inputs = Vec::new();
    let mut it = matches.values_of("input").unwrap();
    while let Some(video_path) = it.next() {
        if video_path == "--" {
            continue;
        }
        let video_path = Path::new(video_path);

        if let Some(arg0) = it.next() {
            if let Some(input_file) = arg0.strip_prefix("@") {
                inputs.push(Input::from_file(video_path,
                    Path::new(input_file)).unwrap());
            } else if arg0 == "--" {
                eprintln!("warning: missing arguments for video file: {:?}", video_path);
                inputs.push(Input::new(video_path));
            } else {
                let mut args = vec![arg0];
                args.extend(it.by_ref().take_while(|s| *s != "--"));
                inputs.push(Input::from_args(video_path,
                    args.into_iter()).unwrap());
            }
        } else {
            eprintln!("warning: missing arguments for video file: {:?}", video_path);
            inputs.push(Input::new(video_path));
        }
    }



    let config = Config {
        width, height, fps, cut_start, cut_end, cmp, pause, inputs
    };

    eprintln!("{:#?}", config);

    let info = config.prepare().unwrap();

    eprintln!("{:#?}", info);
    
    if let Some(name) = output {
        if raw {
            if name == "-" {
                config.render_raw(&info, io::stdout(), report).unwrap();
            } else {
                config.render_raw_to_file(&info, Path::new(name), report).unwrap();
            }
        } else {
            if name == "-" {
                config.encode_to_stdout(&info, report).unwrap();
            } else {
                config.encode_to_file(&info, Path::new(name), report).unwrap();
            }
        }

    } else {
        config.play(&info).unwrap();
    }
}
