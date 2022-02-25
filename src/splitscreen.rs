use std::error::Error;
use std::fmt;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use font_loader::system_fonts;
use image::{GenericImage, RgbImage, Rgb};
use imageproc::drawing::{draw_filled_rect_mut, draw_text_mut};
use imageproc::rect::Rect;
use rusttype::{Font, Scale, point};

#[derive(Debug, Clone)]
pub struct Config {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub cmp: Option<Compare>,
    pub pause: f64,
    pub inputs: Vec<Input>
}

#[derive(Debug, Copy, Clone)]
pub enum Encoder {
    X264,
    VAAPI,
    NVENC,
    AMF,
    QSV
}

#[derive(Debug, Copy, Clone)]
pub enum Compare {
    TimeLoss,
    TimeSave
}

#[derive(Debug, Clone)]
pub struct Input {
    pub video_path: PathBuf,
    pub splits: Vec<f64>
}

#[derive(Debug, Clone)]
pub struct RenderInfo {
    pub start: u32,
    pub length: u32,
    pub tiles: Vec<RenderTileInfo>,
    pub pauses: Vec<u32>
}

#[derive(Debug, Clone)]
pub struct RenderTileInfo {
    pub input: usize,
    pub offset: u32,
    pub length: u32,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub splits: Vec<(u32, u32)>
}



impl Config {
    pub fn prepare(&self) -> Result<RenderInfo, Box<dyn Error>> {
        let ffprobe = find_exec("ffprobe").ok_or("ffprobe not found")?;

        let n_splits = self.inputs[0].splits.len();
        for input in &self.inputs {
            if input.splits.len() != n_splits {
                Err("inputs must have equal number of splits")?;
            }
        }
        if n_splits == 0 {
            Err("inputs need at least one split")?;
        }

        let mut inputs = Vec::new();
        for input in &self.inputs {
            let mut ffprobe = Command::new(&ffprobe)
                .arg("-select_streams").arg("v:0")
                .arg("-show_entries").arg("stream=width,height,duration")
                .arg("-of").arg("default=nw=1:nk=1")
                .arg(&input.video_path)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()?;

            let lines: Vec<_> = BufReader::new(ffprobe.stdout.take().unwrap())
                .lines().take(3).collect::<Result<_, _>>()?;
            if lines.len() != 3 {
                Err(format!("invalid video file: {:?}", input.video_path))?;
            }

            let width: u32 = lines[0].parse()?;
            let height: u32 = lines[1].parse()?;
            let time: f64 = lines[2].parse()?;

            inputs.push((width, height, input.splits[0], time));
        }

        let tiles_x = (1..).filter(|i| i * i >= inputs.len()).next().unwrap() as u32;
        let tiles_y = (inputs.len() as u32 + tiles_x - 1) / tiles_x;

        let box_width = self.width / tiles_x;
        let box_height = self.height / tiles_x;

        let tiles_off_x = self.width / 2 - tiles_x * box_width / 2;
        let tiles_off_y = self.height / 2 - tiles_y * box_height / 2;

        let tiles_last_row =
            if inputs.len() as u32 % tiles_x == 0 {
                tiles_x
            } else {
                inputs.len() as u32 % tiles_x
            };
        let tiles_off_x_last = self.width / 2 - tiles_last_row * box_width / 2;

        let pause = (self.pause * self.fps as f64 + 0.5) as u32;

        let mut tiles: Vec<_> = inputs.into_iter().enumerate()
            .map(|(i, (width, height, first_split, time))| {
                let (w1, h1) = (box_width, height * box_width / width);
                let (w2, h2) = (width * box_height / height, box_height);
                let (width, height) =
                    if w1 <= box_width && h1 <= box_height {
                        (w1, h1)
                    } else {
                        (w2, h2)
                    };

                let tx = i as u32 % tiles_x;
                let ty = i as u32 / tiles_x;

                let tiles_off_x =
                    if ty == tiles_y - 1 {
                        tiles_off_x_last
                    } else {
                        tiles_off_x
                    };

                let offset = first_split as u32 * self.fps;
                let length = (time * self.fps as f64) as u32 - offset;

                RenderTileInfo {
                    input: i,
                    offset,
                    length,
                    x: tiles_off_x + tx * box_width + box_width / 2 - width / 2,
                    y: tiles_off_y + ty * box_height + box_height / 2 - height / 2,
                    width,
                    height,
                    splits: Vec::with_capacity(n_splits)
                }
            })
            .collect();

        let mut start = 0;
        let mut length = 0;
        let mut pauses = Vec::new();

        for i in 0..n_splits {
            let mut t_max = 0;
            for tile in tiles.iter_mut() {
                let input = &self.inputs[tile.input];

                let t_last =
                    if i == 0 {
                        0
                    } else {
                        (input.splits[i - 1] * self.fps as f64 + 0.5) as u32 - tile.offset + 1
                    };
                let t_next = (input.splits[i] * self.fps as f64 + 0.5) as u32 - tile.offset + 1;
                let t_split = t_next - t_last;

                tile.splits.push((length, length + t_split));
                t_max = t_max.max(t_split);
            }

            length += t_max;
            if i == 0 {
                start = length;
            }
            pauses.push(length);
            length += pause;
        }

        Ok(RenderInfo { start, length, tiles, pauses })
    }

    pub fn play(&self, info: &RenderInfo) -> Result<(), Box<dyn Error>> {
        let ffplay_path = find_exec("ffplay").ok_or("ffplay not found")?;

        let mut ffplay = Command::new(&ffplay_path)
            .arg("-f").arg("rawvideo")
            .arg("-pixel_format").arg("rgb24")
            .arg("-video_size").arg(format!("{}x{}", self.width, self.height))
            .arg("-framerate").arg(format!("{}", self.fps))
            .arg("-window_title").arg("SplitScreen Playback")
            .arg("-autoexit")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;
        
        let res = self.render_raw(info, ffplay.stdin.take().unwrap(), false);
        if res.is_ok() {
            let exit = ffplay.wait()?;
            if !exit.success() {
                Err("ffplay exited abnormally")?;
            }
        } else {
            ffplay.kill().ok();
        }
        res
    }

    pub fn encode_to_stdout(&self, info: &RenderInfo, encoder: Encoder, report: bool) -> Result<(), Box<dyn Error>> {
        let mut ffmpeg = self.encode_command(encoder, report)?
            .arg("-")
            .stdout(Stdio::inherit())
            .spawn()?;

        let res = self.render_raw(info, ffmpeg.stdin.take().unwrap(), report);
        if res.is_ok() {
            let exit = ffmpeg.wait()?;
            if !exit.success() {
                Err("ffmpeg exited abnormally")?;
            }
        } else {
            ffmpeg.kill().ok();
        }
        res
    }

    pub fn encode_to_file(&self, info: &RenderInfo, encoder: Encoder, report: bool, output: &Path) -> Result<(), Box<dyn Error>> {
        let mut ffmpeg = self.encode_command(encoder, report)?
            .arg("-y")
            .arg(output)
            .stdout(Stdio::inherit())
            .spawn()?;

        let res = self.render_raw(info, ffmpeg.stdin.take().unwrap(), report);
        if res.is_ok() {
            let exit = ffmpeg.wait()?;
            if !exit.success() {
                Err("ffmpeg exited abnormally")?;
            }
        } else {
            ffmpeg.kill().ok();
        }
        res
    }

    fn encode_command(&self, encoder: Encoder, report: bool) -> Result<Command, Box<dyn Error>> {
        let ffmpeg = find_exec("ffmpeg").ok_or("ffmpeg not found")?;

        let mut cmd = Command::new(&ffmpeg);
        cmd
            .arg("-f").arg("rawvideo")
            .arg("-pixel_format").arg("rgb24")
            .arg("-video_size").arg(format!("{}x{}", self.width, self.height))
            .arg("-framerate").arg(format!("{}", self.fps))
            .arg("-i").arg("-")
            .arg("-f").arg("mp4");

        encoder.apply_args(&mut cmd);

        cmd.stdin(Stdio::piped());
        if report {
            cmd.stderr(Stdio::null());
        } else {
            cmd.stderr(Stdio::inherit());
        }
        eprintln!("{:?}", cmd);

        Ok(cmd)
    }

    pub fn render_raw_to_file(&self, info: &RenderInfo, output: &Path, report: bool) -> Result<(), Box<dyn Error>> {
        self.render_raw(info, File::create(output)?, report)
    }

    pub fn render_raw<W: Write>(&self, info: &RenderInfo, mut output: W, report: bool) -> Result<(), Box<dyn Error>> {
        self.render(info, |(frame_idx, frame)| {
            if report {
                eprintln!("[splitscreen] progress: {}/{}", frame_idx, info.length);
            }
            if let Some(frame) = frame {
                if let Err(err) = output.write_all(frame.as_raw()) {
                    if err.kind() == io::ErrorKind::BrokenPipe {
                        Ok(false)
                    } else {
                        Err(err)?
                    }
                } else {
                    Ok(true)
                }
            } else {
                Ok(true)
            }
        })
    }

    pub fn render<F>(&self, info: &RenderInfo, mut output: F) -> Result<(), Box<dyn Error>>
        where F: FnMut((u32, Option<&RgbImage>)) -> Result<bool, Box<dyn Error>>
    {
        let ffmpeg = find_exec("ffmpeg").ok_or("ffmpeg not found")?;

        let font_prop = system_fonts::FontPropertyBuilder::new()
            .monospace().build();
        let (font_data, _) = system_fonts::get(&font_prop)
            .ok_or("could not find monospace font")?;
        let font: Font = Font::try_from_bytes(&font_data)
            .ok_or("could not find monospace font")?;

        let mut ffmpegs: Vec<_> = Vec::new();
        for tile in info.tiles.iter() {
            ffmpegs.push(Command::new(&ffmpeg)
                .arg("-hwaccel").arg("auto")
                .arg("-ss").arg(format_time(tile.offset as f64 / self.fps as f64))
                .arg("-i").arg(&self.inputs[tile.input].video_path)
                .arg("-c:v").arg("rawvideo")
                .arg("-pix_fmt").arg("rgb24")
                .arg("-vf").arg(format!("scale={}:{}", tile.width, tile.height))
                .arg("-r").arg(format!("{}", self.fps))
                .arg("-f").arg("rawvideo")
                .arg("-")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()?);
        }

        let ffmpegs_channels: Vec<_> = info.tiles.iter().zip(ffmpegs.iter_mut())
            .map(|(tile, ffmpeg)| {
                let (tx, rx) = mpsc::sync_channel(self.fps as usize);
                let mut stdout = ffmpeg.stdout.take().unwrap();
                let tile = tile.clone();
                thread::spawn(move || {
                    let bufsize = tile.width as usize * tile.height as usize * 3;
                    loop {
                        let mut buf = vec![0u8; bufsize];
                        if stdout.read_exact(&mut buf[..]).is_err() {
                            break;
                        }
                        let img = RgbImage::from_raw(tile.width, tile.height, buf).unwrap();
                        if tx.send(img).is_err() {
                            break;
                        }
                    }
                });
                rx
            })
            .collect();

        let mut frame = RgbImage::new(self.width as u32, self.height as u32);
        let mut frame_cmp_start = None;

        for frame_idx in 0..info.length {
            for (tile, channel) in info.tiles.iter().zip(ffmpegs_channels.iter()) {
                let (split_idx, (start, end)) = tile.splits.iter().cloned().enumerate()
                    .filter(|(_, (start, _end))| *start <= frame_idx)
                    .last().unwrap();

                let mut diff = None;

                if frame_idx == start {
                    frame_cmp_start = None;
                }

                if frame_idx < end {
                    if let Ok(buf) = channel.recv() {
                        frame.copy_from(&buf, tile.x, tile.y).unwrap();
                    } else {
                        draw_filled_rect_mut(&mut frame,
                            Rect::at(tile.x as i32, tile.y as i32)
                                .of_size(tile.width, tile.height),
                            Rgb([ 0, 0, 0 ])
                        );
                    }

                } else {
                    if frame_idx == end {
                        for y in tile.y..(tile.y + tile.height) {
                            for x in tile.x..(tile.x + tile.width) {
                                let px = &mut frame[(x, y)];
                                let v = (
                                    0.2989 * px[0] as f64 +
                                    0.5870 * px[1] as f64 +
                                    0.1140 * px[2] as f64
                                ) as u8;
                                px[0] = v;
                                px[1] = v;
                                px[2] = v;
                            }
                        }
                    }

                    if start != 0 {
                        match self.cmp {
                            Some(Compare::TimeLoss) => {
                                if frame_cmp_start.is_none() {
                                    frame_cmp_start = Some(frame_idx);
                                }
                            },
                            Some(Compare::TimeSave) => {
                                diff = Some((true,
                                    frame_idx.min(info.pauses[split_idx]) - end));
                            },
                            _ => {}
                        }
                    }
                }

                if let Some(cmp_start) = frame_cmp_start {
                    diff = Some((false, frame_idx.min(end) - cmp_start));
                }

                if let Some((inv, diff)) = diff {
                    let diff_s = diff as f64 / self.fps as f64;
                    let (text, color) =
                        if diff == 0 {
                            (format_time(diff_s), Rgb([255, 255, 255]))
                        } else if inv {
                            (format!("-{}", format_time(diff_s)), Rgb([0, 192, 0]))
                        } else {
                            (format!("+{}", format_time(diff_s)), Rgb([192, 0, 0]))
                        };
                    let scale = Scale::uniform(64.0);

                    let v_metrics = font.v_metrics(scale);
                    let offset = point(0.0, v_metrics.ascent);

                    let (mut x_min, mut y_min, mut x_max, mut y_max) =
                        (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
                    for next in font.layout(&text, scale, offset) {
                        if let Some(bbox) = next.pixel_bounding_box() {
                            x_min = x_min.min(bbox.min.x);
                            y_min = y_min.min(bbox.min.y);
                            x_max = x_max.max(bbox.max.x);
                            y_max = y_max.max(bbox.max.y);
                        }
                    }
                    let width = (x_max - x_min) as u32;
                    let height = (y_max - y_min) as u32;

                    let border = height / 2;

                    let x = tile.x + tile.width / 2 - width / 2;
                    let y = tile.y + tile.height - 2 * border - height;

                    draw_filled_rect_mut(&mut frame,
                        Rect::at(x as i32 - border as i32, y as i32 - border as i32)
                            .of_size(width as u32 + 2 * border, height as u32 + 2 * border),
                        Rgb([ 0, 0, 0 ])
                    );

                    draw_text_mut(&mut frame, color, x - x_min as u32, y - y_min as u32, scale, &font, &text);
                }
            }

            if frame_idx < info.start {
                if !output((frame_idx, None))? {
                    break;
                }
            } else {
                if !output((frame_idx, Some(&frame)))? {
                    break;
                }
            }
        }

        Ok(())
    }
}



impl Encoder {
    pub fn all() -> Vec<Encoder> {
        vec![
            Encoder::X264,
            Encoder::VAAPI,
            Encoder::NVENC,
            Encoder::AMF,
            Encoder::QSV
        ]
    }

    pub fn apply_args(&self, cmd: &mut Command) {
        match self {
            Encoder::X264 => {
                cmd
                    .arg("-c:v").arg("libx264")
                    .arg("-crf").arg("23");
            },
            Encoder::VAAPI => {
                cmd
                    .arg("-vaapi_device").arg("/dev/dri/renderD128")
                    .arg("-vf").arg("format=nv12,hwupload")
                    .arg("-c:v").arg("h264_vaapi")
                    .arg("-qp").arg("23");
            },
            Encoder::NVENC => {
                cmd
                    .arg("-c:v").arg("h264_nvenc")
                    .arg("-qp").arg("23");
            },
            Encoder::AMF => {
                unimplemented!() //TODO
            },
            Encoder::QSV => {
                unimplemented!() //TODO
            }
        }
    }
}

impl fmt::Display for Encoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Encoder::X264 =>
                write!(f, "x264"),
            Encoder::VAAPI =>
                write!(f, "vaapi"),
            Encoder::NVENC =>
                write!(f, "nvenc"),
            Encoder::AMF =>
                write!(f, "amf"),
            Encoder::QSV =>
                write!(f, "qsv")
        }
    }
}

impl Input {
    pub fn new(video_path: &Path) -> Input {
        Input {
            video_path: PathBuf::from(video_path),
            splits: vec![]
        }
    }

    pub fn from_file(video_path: &Path, path: &Path) -> Result<Input, Box<dyn Error>> {
        let file = File::open(path)
            .map_err(|e| format!("cannot open {}: {}", video_path.display(), e))?;
        let reader = BufReader::new(file);
        let lines: Vec<_> = reader.lines()
            .collect::<Result<_, _>>()?;
        Self::from_args(video_path, lines.into_iter())
    }

    pub fn from_args<I, S>(video_path: &Path, lines: I) -> Result<Input, Box<dyn Error>>
        where I: Iterator<Item = S>, S: AsRef<str>
    {
        let mut res = Input::new(video_path);
        for next in lines {
            let args: Vec<_> = next.as_ref().split(' ').collect();
            match args[0] {
                "split" => {
                    let time_str = args.get(1).ok_or("missing split time")?;
                    res.splits.push(parse_split_time(time_str)?);
                },
                s =>
                    eprintln!("warning: unknown field `{}`", s)
            }
        }
        Ok(res)
    }
}



pub fn parse_split_time(time_str: &str) -> Result<f64, Box<dyn Error>> {
    let split = time_str.split(':').collect::<Vec<_>>();
    let (h_str, m_str, s_str) =
        if split.len() == 1 {
            ("0", "0", split[0])
        } else if split.len() == 2 {
            ("0", split[0], split[1])
        } else if split.len() == 3 {
            (split[0], split[1], split[2])
        } else {
            panic!()
        };

    let h: usize = h_str.parse()?;
    let m: usize = m_str.parse()?;
    let s: f64 = s_str.parse()?;
    if m > 60 || s < 0.0 || s > 60.0 {
        Err(format!("invalid time: {}", time_str))?;
    }

    Ok(((h * 60 + m) * 60) as f64 + s)
}

pub fn format_time(time: f64) -> String {
    if time < 0.0 {
        return format!("-{}", format_time(-time));
    }

    let ms_total = (time * 1000.0) as u64;
    let ms = ms_total % 1000;
    let s_total = ms_total / 1000;
    if s_total < 60 {
        return format!("{:0>2}.{:0>3}", s_total, ms);
    }

    let s = s_total % 60;
    let m_total = s_total / 60;
    if m_total < 60 {
        return format!("{:0>2}:{:0>2}.{:0>3}", m_total, s, ms);
    }

    let m = m_total % 60;
    let h_total = m_total / 60;
    format!("{:0>2}:{:0>2}:{:0>2}.{:0>3}", h_total, m, s, ms)
}

fn find_exec(name: &str) -> Option<PathBuf> {
    let mut paths = Vec::new();
    let name_exe = name.to_string() + ".exe";

    if let Ok(path) = std::env::current_exe() {
        let path = path.parent().unwrap();
        paths.push(path.join(name));
        paths.push(path.join(&name_exe));
    }

    paths.push(Path::new(".").join(name));
    paths.push(Path::new(".").join(&name_exe));

    paths.push(PathBuf::from(name));
    paths.push(PathBuf::from(&name_exe));

    for next in paths {
        let res = Command::new(&next)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if res.is_ok() {
            return Some(next);
        }
    }

    return None;
}
