//! primitive-gpu: rebuilds a picture from shapes and scores candidate
//! shapes on the GPU.

use image::codecs::gif::{GifEncoder, Repeat};
use image::codecs::jpeg::JpegEncoder;
use image::{Delay, Frame};
use primitive_gpu::core::Metric;
use primitive_gpu::load::{self, parse_hex, pixmap_to_rgba};
use primitive_gpu::model::{self, Model, SearchConfig};
use primitive_gpu::shapes;
use std::io::{Read, Write};
use std::process::exit;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy)]
struct ShapeConfig {
    count: usize,
    mode: i32,
    alpha: i32,
    repeat: usize,
}

struct Args {
    input: String,
    outputs: Vec<String>,
    background: String,
    configs: Vec<ShapeConfig>,
    alpha: i32,
    input_size: u32,
    output_size: u32,
    mode: i32,
    workers: usize,
    nth: usize,
    repeat: usize,
    /// Stop once the score is at or below this. 0 = off.
    target_score: f64,
    log: u32,
    search: SearchConfig,
    seed: Option<u64>,
}

fn usage() -> ! {
    eprintln!(
        "Usage: primitive-gpu [OPTIONS] -i input -o output -n count

  -i <path>      input image path (\"-\" reads stdin)
  -o <path>      output image path; .png .jpg .svg .gif; repeatable;
                 put %d in the name to save every frame; \"-\" writes SVG to stdout
  -n <count>     number of primitives; repeatable (each uses the -m/-a/-rep given before it)
  -m <mode>      {} (default 1)
  -a <alpha>     alpha value, 0 lets the search choose (default 128)
  -bg <hex>      background color (default: average of the image)
  -r <size>      resize large input images to this size (default 256)
  -s <size>      output image size (default 1024)
  -j <count>     CPU threads for rasterizing (default: all cores)
  -nth <n>       save every Nth frame (with %d in the path) (default 1)
  -rep <n>       add N extra shapes per iteration with reduced search (default 0)
  -metric <name> how the gap to the original is counted: squared (default)
                 or plain (no squaring: every mistake counts the same,
                 gives flatter, bolder shapes; the score is then the
                 average gap, so -score targets differ)
  -score <x>     stop early once the score is at or below x, e.g. 0.03;
                 -n is then the most shapes (default: off)
  -v, -vv        verbose / very verbose

GPU search tuning:
  -chains <n>    hill-climb chains per shape (default 16)
  -random <n>    random tries that seed each chain (default 1000)
  -age <n>       tries without improvement before a chain stops (default 100)
  -k <n>         mutations each chain tries per GPU round (default 32)
  -seed <n>      random seed (default: from the clock)
  -raster <cpu|gpu>  who draws the candidate shapes (default gpu); cpu is
                 the older path, kept for comparison
  -check         also score every candidate exactly on the CPU and print
                 how far the GPU scores are off (slow)",
        shapes::MODE_NAMES
    );
    exit(1)
}

fn parse_args() -> Args {
    let mut a = Args {
        input: String::new(),
        outputs: Vec::new(),
        background: String::new(),
        configs: Vec::new(),
        alpha: 128,
        input_size: 256,
        output_size: 1024,
        mode: 1,
        workers: 0,
        nth: 1,
        repeat: 0,
        target_score: 0.0,
        log: 0,
        search: SearchConfig { chains: 16, random: 1000, age: 100, k: 32, gpu_draw: true, check: false, metric: Metric::Squared },
        seed: None,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let raw = &argv[i];
        i += 1;
        if !raw.starts_with('-') || raw == "-" {
            eprintln!("ERROR: unexpected argument: {raw}");
            usage();
        }
        let flag = raw.trim_start_matches('-');
        let (name, inline) = match flag.split_once('=') {
            Some((n, v)) => (n.to_string(), Some(v.to_string())),
            None => (flag.to_string(), None),
        };
        match name.as_str() {
            "h" | "help" => usage(),
            "check" => {
                a.search.check = inline.as_deref().is_none_or(|v| v == "true" || v == "1");
                continue;
            }
            "v" | "vv" => {
                let on = inline.as_deref().is_none_or(|v| v == "true" || v == "1");
                if on {
                    a.log = a.log.max(if name == "v" { 1 } else { 2 });
                }
                continue;
            }
            _ => {}
        }
        let value = match inline {
            Some(v) => v,
            None => {
                if i >= argv.len() {
                    eprintln!("ERROR: flag needs an argument: -{name}");
                    usage();
                }
                i += 1;
                argv[i - 1].clone()
            }
        };
        let num = |v: &str| -> i64 {
            v.parse().unwrap_or_else(|_| {
                eprintln!("ERROR: invalid value \"{v}\" for -{name}");
                usage()
            })
        };
        match name.as_str() {
            "i" => a.input = value,
            "o" => a.outputs.push(value),
            "n" => {
                let c = num(&value).max(0) as usize;
                a.configs.push(ShapeConfig { count: c, mode: a.mode, alpha: a.alpha, repeat: a.repeat });
            }
            "bg" => a.background = value,
            "a" => a.alpha = num(&value) as i32,
            "r" => a.input_size = num(&value).max(0) as u32,
            "s" => a.output_size = num(&value).max(1) as u32,
            "m" => a.mode = num(&value) as i32,
            "j" => a.workers = num(&value).max(0) as usize,
            "nth" => a.nth = num(&value).max(1) as usize,
            "rep" => a.repeat = num(&value).max(0) as usize,
            "score" => {
                a.target_score = value.parse().unwrap_or_else(|_| {
                    eprintln!("ERROR: invalid value \"{value}\" for -score");
                    usage()
                });
                if !(a.target_score > 0.0 && a.target_score < 1.0) {
                    eprintln!("ERROR: -score must be between 0 and 1");
                    usage();
                }
            }
            "metric" => {
                a.search.metric = Metric::parse(&value).unwrap_or_else(|| {
                    eprintln!("ERROR: -metric must be squared or plain");
                    usage()
                })
            }
            "chains" => a.search.chains = num(&value).max(1) as usize,
            "random" => a.search.random = num(&value).max(1) as usize,
            "age" => a.search.age = num(&value).max(1) as usize,
            "k" => a.search.k = num(&value).max(1) as usize,
            "seed" => a.seed = Some(num(&value) as u64),
            "raster" => match value.as_str() {
                "cpu" => a.search.gpu_draw = false,
                "gpu" => a.search.gpu_draw = true,
                _ => {
                    eprintln!("ERROR: -raster must be cpu or gpu");
                    usage();
                }
            },
            _ => {
                eprintln!("ERROR: unknown flag: -{name}");
                usage();
            }
        }
    }
    let mut ok = true;
    if a.input.is_empty() {
        eprintln!("ERROR: input argument required");
        ok = false;
    }
    if a.outputs.is_empty() {
        eprintln!("ERROR: output argument required");
        ok = false;
    }
    if a.configs.is_empty() {
        eprintln!("ERROR: number argument required");
        ok = false;
    }
    if a.configs.len() == 1 {
        a.configs[0].mode = a.mode;
        a.configs[0].alpha = a.alpha;
        a.configs[0].repeat = a.repeat;
    }
    for c in &a.configs {
        if c.count < 1 {
            eprintln!("ERROR: number argument must be > 0");
            ok = false;
        }
        if !(0..=8).contains(&c.mode) {
            eprintln!("ERROR: mode must be 0-8");
            ok = false;
        }
        if !(0..=255).contains(&c.alpha) {
            eprintln!("ERROR: alpha must be 0-255");
            ok = false;
        }
    }
    if !ok {
        usage();
    }
    a
}

fn number_string(mut x: f64) -> String {
    for suffix in ["", "k", "M", "G"] {
        if x < 1000.0 {
            return format!("{x:.1}{suffix}");
        }
        x /= 1000.0;
    }
    format!("{x:.1}T")
}

/// Fills in the frame number: handles %d and %0Nd.
fn frame_path(path: &str, frame: usize) -> String {
    if let Some(p) = path.find('%') {
        let rest = &path[p + 1..];
        if let Some(d) = rest.find('d') {
            let spec = &rest[..d];
            if spec.chars().all(|c| c.is_ascii_digit()) {
                let width: usize = spec.parse().unwrap_or(0);
                let num = if spec.starts_with('0') { format!("{frame:0width$}") } else { format!("{frame:width$}") };
                return format!("{}{}{}", &path[..p], num, &rest[d + 1..]);
            }
        }
    }
    path.to_string()
}

fn save(model: &Model, path: &str, ext: &str) -> Result<(), String> {
    let err = |e: &dyn std::fmt::Display| format!("{path}: {e}");
    match ext {
        ".png" => pixmap_to_rgba(&model.canvas).save(path).map_err(|e| err(&e)),
        ".jpg" | ".jpeg" => {
            let rgb = image::DynamicImage::ImageRgba8(pixmap_to_rgba(&model.canvas)).to_rgb8();
            let f = std::fs::File::create(path).map_err(|e| err(&e))?;
            JpegEncoder::new_with_quality(std::io::BufWriter::new(f), 95)
                .encode_image(&rgb)
                .map_err(|e| err(&e))
        }
        ".svg" => {
            let svg = model.svg();
            if path == "-" {
                std::io::stdout().write_all(svg.as_bytes()).map_err(|e| err(&e))
            } else {
                std::fs::write(path, svg).map_err(|e| err(&e))
            }
        }
        ".gif" => {
            let frames = model.frames(0.001);
            let f = std::fs::File::create(path).map_err(|e| err(&e))?;
            let mut enc = GifEncoder::new_with_speed(std::io::BufWriter::new(f), 10);
            enc.set_repeat(Repeat::Infinite).map_err(|e| err(&e))?;
            let last = frames.len() - 1;
            for (i, pm) in frames.iter().enumerate() {
                let ms = if i == last { 2500 } else { 500 };
                let frame = Frame::from_parts(pixmap_to_rgba(pm), 0, 0, Delay::from_numer_denom_ms(ms, 1));
                enc.encode_frame(frame).map_err(|e| err(&e))?;
            }
            Ok(())
        }
        _ => Err(format!("unrecognized file extension: {ext}")),
    }
}

fn main() {
    let args = parse_args();
    let log = args.log;
    macro_rules! v {
        ($($t:tt)*) => { if log >= 1 { print!($($t)*); let _ = std::io::stdout().flush(); } };
    }

    if args.workers > 0 {
        let _ = rayon::ThreadPoolBuilder::new().num_threads(args.workers).build_global();
    }
    let seed = args.seed.unwrap_or_else(|| {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1)
    });

    // read input image
    v!("reading {}\n", args.input);
    let img = if args.input == "-" {
        let mut buf = Vec::new();
        if let Err(e) = std::io::stdin().read_to_end(&mut buf) {
            eprintln!("ERROR: reading stdin: {e}");
            exit(1);
        }
        image::load_from_memory(&buf).map_err(|e| format!("stdin: {e}"))
    } else {
        load::open_image(&args.input)
    };
    let img = img.unwrap_or_else(|e| {
        eprintln!("ERROR: {e}");
        exit(1)
    });

    // scale down input image if needed (like resize.Thumbnail)
    let img = load::shrink(img, args.input_size);
    let (target, w, h) = load::premultiplied(&img);

    // determine background color
    let bg = if args.background.is_empty() { load::average_color(&target) } else { parse_hex(&args.background) };

    let check = args.search.check;
    let mut model = Model::new(target, w, h, bg, args.output_size, args.search, seed).unwrap_or_else(|e| {
        eprintln!("ERROR: {e}");
        exit(1)
    });
    v!("gpu: {}\n", model.gpu_name());
    v!("{}: t={:.3}, score={:.6}\n", 0, 0.0, model.score);

    let start = Instant::now();
    let mut frame = 0usize;
    let nconf = args.configs.len();
    let target_score = args.target_score;
    'configs: for (j, config) in args.configs.iter().enumerate() {
        v!("count={}, mode={}, alpha={}, repeat={}\n", config.count, config.mode, config.alpha, config.repeat);
        for i in 0..config.count {
            frame += 1;
            let t = Instant::now();
            let n = model.step(config.mode, config.alpha, config.repeat);
            let nps = number_string(n as f64 / t.elapsed().as_secs_f64());
            let elapsed = start.elapsed().as_secs_f64();
            v!("{}: t={:.3}, score={:.6}, n={}, n/s={}\n", frame, elapsed, model.score, n, nps);
            if log >= 2 {
                println!("  shapes={}, last={:?}", model.shapes.len(), model.shapes.last());
            }
            let reached = target_score > 0.0 && model.score <= target_score;

            for output in &args.outputs {
                let ext = if output == "-" {
                    ".svg".to_string()
                } else {
                    std::path::Path::new(output)
                        .extension()
                        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
                        .unwrap_or_default()
                };
                let percent = output.contains('%');
                let save_frames = percent && ext != ".gif" && frame.is_multiple_of(args.nth);
                let last = reached || (j == nconf - 1 && i == config.count - 1);
                if save_frames || last {
                    let path = if percent { frame_path(output, frame) } else { output.clone() };
                    v!("writing {}\n", path);
                    if let Err(e) = save(&model, &path, &ext) {
                        eprintln!("ERROR: {e}");
                        exit(1);
                    }
                }
            }
            if reached {
                v!("reached score {} with {} shapes\n", target_score, model.shapes.len());
                break 'configs;
            }
        }
    }
    if check {
        print_check(&model.check);
    }
    if log >= 2 {
        print_timings(&model.times, start.elapsed().as_secs_f64(), rayon::current_num_threads());
    }
}

fn print_check(c: &model::CheckStats) {
    let n = c.candidates.max(1) as f64;
    println!("check: {} candidates in {} batches", c.candidates, c.batches);
    println!(
        "  score differs from exact: {} candidates ({:.2}%), mean |diff| {:.1}, max |diff| {}, mean |exact| {:.1} (diff/exact {:.5}%)",
        c.differ,
        100.0 * c.differ as f64 / n,
        c.sum_abs_diff / n,
        c.max_abs_diff,
        c.sum_abs_exact / n,
        100.0 * c.sum_abs_diff / c.sum_abs_exact.max(1.0)
    );
    println!(
        "  batch winner differs: {} of {} batches; loss {:.5}% of the winners' gain",
        c.winner_differs,
        c.batches,
        100.0 * c.winner_loss / c.winner_gain.max(1.0)
    );
}

/// Timing breakdown for `-vv`. CPU columns are summed over all threads and
/// also shown divided by the thread count, so they compare to wall time.
fn print_timings(t: &model::Timings, wall: f64, threads: usize) {
    let ms = |s: f64| s * 1000.0;
    println!("timing (ms; cpu = summed over {threads} threads, /thr = cpu divided by threads):");
    println!(
        "  {:<7} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>9} {:>9}",
        "phase", "wall", "prepare", "make", "draw", "pack", "draw/thr", "upload", "gpu-wait", "other", "cand/s", "bytes/cand"
    );
    for (name, p) in [("random", &t.random), ("climb", &t.climb)] {
        let other = p.total - p.prepare - p.upload - p.wait;
        let cps = if p.total > 0.0 { p.candidates as f64 / p.total } else { 0.0 };
        let bpc = if p.candidates > 0 { p.bytes as f64 / p.candidates as f64 } else { 0.0 };
        println!(
            "  {:<7} {:>8.1} {:>8.1} {:>8.1} {:>8.1} {:>8.1} {:>8.1} {:>8.1} {:>8.1} {:>8.1} {:>9} {:>9.0}",
            name,
            ms(p.total),
            ms(p.prepare),
            ms(p.make),
            ms(p.draw),
            ms(p.pack),
            ms(p.draw / threads as f64),
            ms(p.upload),
            ms(p.wait),
            ms(other),
            number_string(cps),
            bpc
        );
    }
    let cand = t.random.candidates + t.climb.candidates;
    println!(
        "  commit {:.1} ms, total wall {:.1} ms, candidates {} ({}/s), batches {}+{}",
        ms(t.commit),
        ms(wall),
        cand,
        number_string(cand as f64 / wall),
        t.random.batches,
        t.climb.batches
    );
}
