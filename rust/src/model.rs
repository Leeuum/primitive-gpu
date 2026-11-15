//! The model: target, current canvas, shape history, and the search.
//!
//! The search runs in big batches so the
//! GPU can score many candidates at once:
//!   1. `chains` groups of `random` random shapes are scored in one batch;
//!      the best of each group seeds one hill-climbing chain.
//!   2. All chains climb in lockstep. Each round, every live chain tries
//!      `k` mutations of its current shape; the best one is kept if it beats
//!      the chain. A chain stops after `age` tries without improvement.
//!   3. The best chain's shape is added to the canvas.

use crate::core::{Metric, compute_color, delta_lines, difference_full, difference_partial, draw_lines, score_of};
use crate::gpu::{Gpu, GpuCand, GpuDesc, GpuSpan, SLOTS};
use std::collections::VecDeque;
use std::sync::mpsc;
use crate::raster::Scratch;
use crate::rng::Rng;
use crate::shapes::Shape;
use rayon::prelude::*;
use std::time::Instant;
use tiny_skia::{Color as SkColor, Paint, Pixmap, Transform};

#[derive(Clone, Copy, Debug)]
pub struct State {
    pub shape: Shape,
    pub alpha: i32,
    pub mutate_alpha: bool,
}

impl State {
    fn new(shape: Shape, alpha: i32) -> State {
        if alpha == 0 {
            State { shape, alpha: 128, mutate_alpha: true }
        } else {
            State { shape, alpha, mutate_alpha: false }
        }
    }

    fn do_move(&mut self, w: i32, h: i32, rng: &mut Rng) {
        self.shape.mutate(w, h, rng);
        if self.mutate_alpha {
            self.alpha = (self.alpha + rng.intn(21) - 10).clamp(1, 255);
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SearchConfig {
    pub chains: usize,
    pub random: usize,
    pub age: usize,
    pub k: usize,
    /// Draw candidates on the GPU where the shape kind allows it.
    pub gpu_draw: bool,
    /// Also score every batch exactly on the CPU and compare.
    pub check: bool,
    /// How gaps are counted. Fixed when the model is made (the GPU
    /// programs are built for it); `set_search` keeps it.
    pub metric: Metric,
}

struct Batch {
    states: Vec<State>,
    span_chunks: Vec<Vec<GpuSpan>>,
    descs: Vec<GpuDesc>,
    /// Shape records; non-empty means the GPU-drawing path.
    cands: Vec<GpuCand>,
    /// Sizes of the batches that were joined into this one. `-check` keeps
    /// its numbers per original batch.
    parts: Vec<usize>,
}

/// One CPU task's share of a batch.
#[derive(Default)]
struct Part {
    states: Vec<State>,
    spans: Vec<GpuSpan>,
    descs: Vec<GpuDesc>,
    /// CPU seconds: make, draw, pack.
    tm: [f64; 3],
}

/// `-check` results: GPU error change compared with the exact CPU one.
#[derive(Default, Clone, Copy, Debug)]
pub struct CheckStats {
    pub candidates: usize,
    /// Candidates whose GPU and CPU error change differ at all.
    pub differ: usize,
    pub sum_abs_diff: f64,
    pub max_abs_diff: i64,
    pub sum_abs_exact: f64,
    pub batches: usize,
    /// Batches where the GPU's best candidate is not the CPU's best.
    pub winner_differs: usize,
    /// Summed exact loss from picking the GPU's best instead of the CPU's.
    pub winner_loss: f64,
    /// Summed exact improvement of the CPU's best (to compare the loss to).
    pub winner_gain: f64,
}

impl CheckStats {
    fn add(&mut self, gpu: &[i64], exact: &[i64]) {
        if gpu.is_empty() {
            return;
        }
        for (&g, &e) in gpu.iter().zip(exact) {
            let d = (g - e).abs();
            self.candidates += 1;
            self.differ += (d != 0) as usize;
            self.sum_abs_diff += d as f64;
            self.max_abs_diff = self.max_abs_diff.max(d);
            self.sum_abs_exact += e.abs() as f64;
        }
        let argmin = |v: &[i64]| v.iter().enumerate().min_by_key(|(_, &x)| x).map(|(i, _)| i).unwrap();
        let (gi, ei) = (argmin(gpu), argmin(exact));
        self.batches += 1;
        if exact[gi] != exact[ei] {
            self.winner_differs += 1;
            self.winner_loss += (exact[gi] - exact[ei]) as f64;
        }
        self.winner_gain += (-exact[ei]).max(0) as f64;
    }
}

/// Time spent in one search phase (random or climb), for `-vv`.
/// `make`, `draw` and `pack` are CPU time summed over all threads;
/// the rest are wall-clock time on the main thread.
#[derive(Default, Clone, Copy, Debug)]
pub struct PhaseTimes {
    /// Wall time of the whole phase.
    pub total: f64,
    /// Wall time inside `prepare` (the parallel CPU work plus merging).
    pub prepare: f64,
    /// CPU time creating or mutating shapes.
    pub make: f64,
    /// CPU time rasterizing shapes into scanlines.
    pub draw: f64,
    /// CPU time turning scanlines into GPU spans.
    pub pack: f64,
    /// Wall time copying the batch into the GPU upload area and submitting.
    pub upload: f64,
    /// Wall time the CPU waits for GPU results.
    pub wait: f64,
    pub candidates: usize,
    pub batches: usize,
    pub bytes: u64,
}

#[derive(Default, Clone, Copy, Debug)]
pub struct Timings {
    pub random: PhaseTimes,
    pub climb: PhaseTimes,
    /// Wall time committing shapes (`Model::add`).
    pub commit: f64,
}

/// Batches up to this size are prepared on the calling thread.
const SERIAL_MAX: usize = 1024;

/// Make several batches of GPU shape records and join them into one batch.
/// Batch `b` holds `counts[b]` states made by `gen(b, i, rng)` and uses
/// `seeds[b]`. Each batch is cut into pieces with their own random
/// generators, so the shapes do not depend on how the batches are joined.
/// Returns the batch and the CPU seconds spent.
fn make_cands<F>(w: i32, h: i32, counts: &[usize], seeds: &[u64], serial: bool, gen: F) -> (Batch, f64)
where
    F: Fn(usize, usize, &mut Rng) -> State + Sync,
{
    let total: usize = counts.iter().sum();
    let filler = State { shape: Shape::Rectangle { x1: 0, y1: 0, x2: 0, y2: 0 }, alpha: 0, mutate_alpha: false };
    let mut states = vec![filler; total];
    let mut cands = vec![GpuCand::default(); total];
    // One task per piece: (batch, seed, first index in the batch, outputs).
    let mut tasks: Vec<(usize, u64, usize, &mut [State], &mut [GpuCand])> = Vec::new();
    let (mut srest, mut crest) = (states.as_mut_slice(), cands.as_mut_slice());
    for (b, (&n, &seed)) in counts.iter().zip(seeds).enumerate() {
        let chunk = (n / (rayon::current_num_threads() * 4)).clamp(1, 64);
        let (sb, sr) = srest.split_at_mut(n);
        let (cb, cr) = crest.split_at_mut(n);
        (srest, crest) = (sr, cr);
        for (c, (ss, cs)) in sb.chunks_mut(chunk).zip(cb.chunks_mut(chunk)).enumerate() {
            tasks.push((b, seed ^ (c as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15), c * chunk, ss, cs));
        }
    }
    let fill = |(b, seed, base, ss, cs): (usize, u64, usize, &mut [State], &mut [GpuCand])| {
        let mut rng = Rng::new(seed);
        for (j, (s, cd)) in ss.iter_mut().zip(cs.iter_mut()).enumerate() {
            let st = gen(b, base + j, &mut rng);
            *cd = st.shape.gpu_record(w, h, st.alpha).expect("shape kind not drawable on GPU");
            *s = st;
        }
    };
    let ta = Instant::now();
    let cpu = if serial {
        tasks.into_iter().for_each(fill);
        ta.elapsed().as_secs_f64()
    } else {
        tasks
            .into_par_iter()
            .with_min_len(4)
            .map(|t| {
                let ta = Instant::now();
                fill(t);
                ta.elapsed().as_secs_f64()
            })
            .sum()
    };
    (Batch { states, span_chunks: Vec::new(), descs: Vec::new(), cands, parts: counts.to_vec() }, cpu)
}

/// What decides the random shapes of one step: (shape mode, alpha, how many).
type RandomKey = (i32, i32, usize);

/// How many shapes go in each batch of the random phase.
fn random_counts(n: usize) -> Vec<usize> {
    let parts = n.div_ceil(2000).clamp(1, 8);
    let per = n.div_ceil(parts);
    (0..parts).map(|p| per.min(n - (p * per).min(n))).filter(|&c| c > 0).collect()
}

/// The random shapes of one step on the GPU-drawing path, as one or two GPU
/// jobs, with the CPU seconds spent. Needs one seed per `random_counts`
/// entry. Random shapes do not depend on the canvas, so this can run before
/// the step it is for.
fn random_jobs(w: i32, h: i32, (shape_type, alpha, n): RandomKey, seeds: &[u64]) -> (Vec<Batch>, f64) {
    let counts = random_counts(n);
    let mid = counts.len().div_ceil(2);
    let mut jobs = Vec::new();
    let mut cpu = 0.0;
    for (cs, ss) in [(&counts[..mid], &seeds[..mid]), (&counts[mid..], &seeds[mid..])] {
        if !cs.is_empty() {
            let (b, t) = make_cands(w, h, cs, ss, false, |_, _, rng| State::new(Shape::random(shape_type, w, h, rng), alpha));
            jobs.push(b);
            cpu += t;
        }
    }
    (jobs, cpu)
}

/// Canvas size and scale for a picture of `w` x `h` drawn with a long side
/// of `size`: (width, height, scale).
pub fn output_size(w: i32, h: i32, size: u32) -> (u32, u32, f64) {
    let aspect = w as f64 / h as f64;
    if aspect >= 1.0 {
        (size, (size as f64 / aspect) as u32, size as f64 / w as f64)
    } else {
        ((size as f64 * aspect) as u32, size, size as f64 / h as f64)
    }
}

/// A working-size canvas filled with the background color (premultiplied).
fn blank_current(w: i32, h: i32, bg: [u8; 4]) -> Vec<u8> {
    let premul = |c: u8| (c as u32 * bg[3] as u32 / 255) as u8;
    let px = [premul(bg[0]), premul(bg[1]), premul(bg[2]), bg[3]];
    px.iter().copied().cycle().take((w * h * 4) as usize).collect()
}

/// An output canvas filled with the background color.
pub fn blank_canvas(sw: u32, sh: u32, bg: [u8; 4]) -> Pixmap {
    let mut pm = Pixmap::new(sw, sh).expect("output size must be > 0");
    pm.fill(SkColor::from_rgba8(bg[0], bg[1], bg[2], bg[3]));
    pm
}

fn shape_transform(scale: f64) -> Transform {
    let s = scale as f32;
    Transform::from_scale(s, s).pre_translate(0.5, 0.5)
}

fn shape_paint(c: [u8; 4]) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(c[0], c[1], c[2], c[3]);
    p.anti_alias = true;
    p
}

/// Draw shapes with their colors on an output canvas, `scale` output
/// pixels per working pixel.
pub fn draw_shapes(pm: &mut Pixmap, shapes: &[Shape], colors: &[[u8; 4]], scale: f64) {
    let t = shape_transform(scale);
    for (s, &c) in shapes.iter().zip(colors) {
        s.draw(pm, &shape_paint(c), t);
    }
}

/// The shapes as an SVG file of `sw` x `sh`, `scale` output pixels per
/// working pixel.
pub fn svg_of(sw: u32, sh: u32, scale: f64, bg: [u8; 4], shapes: &[Shape], colors: &[[u8; 4]]) -> String {
    let mut lines = vec![
        format!("<svg xmlns=\"http://www.w3.org/2000/svg\" version=\"1.1\" width=\"{}\" height=\"{}\">", sw, sh),
        format!(
            "<rect x=\"0\" y=\"0\" width=\"{}\" height=\"{}\" fill=\"#{:02x}{:02x}{:02x}\" />",
            sw, sh, bg[0], bg[1], bg[2]
        ),
        format!("<g transform=\"scale({:.6}) translate(0.5 0.5)\">", scale),
    ];
    for (s, c) in shapes.iter().zip(colors) {
        let attrs = format!(
            "fill=\"#{:02x}{:02x}{:02x}\" fill-opacity=\"{:.6}\"",
            c[0],
            c[1],
            c[2],
            c[3] as f64 / 255.0
        );
        lines.push(s.svg(&attrs));
    }
    lines.push("</g>".into());
    lines.push("</svg>".into());
    lines.join("\n")
}

struct Chain {
    state: State,
    energy: f64,
    age: usize,
}

pub struct Model {
    pub w: i32,
    pub h: i32,
    pub sw: u32,
    pub sh: u32,
    pub scale: f64,
    pub bg: [u8; 4],
    target: Vec<u8>,
    current: Vec<u8>,
    total: u64,
    pub score: f64,
    pub shapes: Vec<Shape>,
    pub colors: Vec<[u8; 4]>,
    pub scores: Vec<f64>,
    pub canvas: Pixmap,
    gpu: Gpu,
    rng: Rng,
    scratch: Scratch,
    pub counter: usize,
    cfg: SearchConfig,
    pub times: Timings,
    pub check: CheckStats,
    check_pending: [Option<(Vec<i64>, Vec<usize>)>; SLOTS],
    /// GPU drawing is used for the current step.
    gpu_draw_now: bool,
    /// The current step sends few, large GPU jobs and prepares small batches
    /// on one thread (see `climb_joined`). Only a speed choice: the shapes
    /// and scores are the same either way.
    joined: bool,
    /// Which phase the timers currently add to: false = random, true = climb.
    climbing: bool,
    /// Random shapes for the next step, being made in the background.
    pregen: Option<(RandomKey, mpsc::Receiver<(Vec<Batch>, f64)>)>,
}

impl Model {
    pub fn new(target: Vec<u8>, w: i32, h: i32, bg: [u8; 4], size: u32, cfg: SearchConfig, seed: u64) -> Result<Model, String> {
        let (sw, sh, scale) = output_size(w, h, size);
        let current = blank_current(w, h, bg);
        let m = cfg.metric;
        let total = difference_full(m, &target, &current);
        let gpu = Gpu::new(&target, &current, w as u32, h as u32, m)?;
        let canvas = Self::new_canvas(sw.max(1), sh.max(1), bg);
        Ok(Model {
            w,
            h,
            sw,
            sh,
            scale,
            bg,
            target,
            current,
            total,
            score: score_of(m, total, w, h),
            shapes: Vec::new(),
            colors: Vec::new(),
            scores: Vec::new(),
            canvas,
            gpu,
            rng: Rng::new(seed),
            scratch: Scratch::default(),
            counter: 0,
            cfg,
            times: Timings::default(),
            check: CheckStats::default(),
            check_pending: Default::default(),
            gpu_draw_now: false,
            joined: false,
            climbing: false,
            pregen: None,
        })
    }

    pub fn gpu_name(&self) -> &str {
        &self.gpu.adapter_name
    }

    fn new_canvas(sw: u32, sh: u32, bg: [u8; 4]) -> Pixmap {
        blank_canvas(sw, sh, bg)
    }

    fn base_transform(&self) -> Transform {
        shape_transform(self.scale)
    }

    fn paint(c: [u8; 4]) -> Paint<'static> {
        shape_paint(c)
    }

    /// The working-size canvas (premultiplied RGBA8).
    pub fn current(&self) -> &[u8] {
        &self.current
    }

    /// The working-size target picture (premultiplied RGBA8).
    pub fn target(&self) -> &[u8] {
        &self.target
    }

    /// Change the search tuning between steps. The `check` and `gpu_draw`
    /// fields are kept as they are.
    pub fn set_search(&mut self, cfg: SearchConfig) {
        self.cfg = SearchConfig { check: self.cfg.check, gpu_draw: self.cfg.gpu_draw, metric: self.cfg.metric, ..cfg };
    }

    /// Swap in a new picture of the same size (the next frame of a video)
    /// and start again from `start` (the working canvas and the preview
    /// canvas), or from the background color when `start` is None. Clears
    /// the shape list. The random shapes made in the background are kept:
    /// they depend only on the size, mode and alpha.
    pub fn set_target(&mut self, target: Vec<u8>, bg: [u8; 4], start: Option<(Vec<u8>, Pixmap)>) {
        assert_eq!(target.len(), self.target.len(), "set_target needs a picture of the same size");
        self.target = target;
        self.bg = bg;
        self.reset(start);
        self.gpu.set_target(&self.target, &self.current);
    }

    /// Start again from `start`, or from the background color when it is
    /// None, and clear the shape list. Keeps the target picture. Does not
    /// upload anything to the GPU; the caller does that.
    fn reset(&mut self, start: Option<(Vec<u8>, Pixmap)>) {
        let bg = self.bg;
        let (current, canvas) = match start {
            Some((c, p)) => (c, p),
            None => (blank_current(self.w, self.h, bg), Self::new_canvas(self.sw.max(1), self.sh.max(1), bg)),
        };
        self.current = current;
        self.canvas = canvas;
        self.total = difference_full(self.cfg.metric, &self.target, &self.current);
        self.score = score_of(self.cfg.metric, self.total, self.w, self.h);
        self.shapes.clear();
        self.colors.clear();
        self.scores.clear();
    }

    /// Put back saved shapes after `set_target`: draws them with the exact
    /// CPU code and the saved colors (no search), then uploads the canvas
    /// once.
    pub fn load_shapes(&mut self, shapes: &[Shape], colors: &[[u8; 4]], scores: &[f64]) {
        let (w, h) = (self.w, self.h);
        let t = self.base_transform();
        for (s, &c) in shapes.iter().zip(colors) {
            s.rasterize(w, h, &mut self.scratch);
            draw_lines(&mut self.current, w, c, &self.scratch.lines);
            s.draw(&mut self.canvas, &Self::paint(c), t);
        }
        self.shapes.extend_from_slice(shapes);
        self.colors.extend_from_slice(colors);
        self.scores.extend_from_slice(scores);
        self.total = difference_full(self.cfg.metric, &self.target, &self.current);
        self.score = score_of(self.cfg.metric, self.total, w, h);
        self.gpu.set_current(&self.current);
    }

    /// Keep only the first `n` shapes. Starts again from the background
    /// color, so it is only right when the picture started from it.
    pub fn truncate(&mut self, n: usize) {
        if n >= self.shapes.len() {
            return;
        }
        let shapes: Vec<Shape> = self.shapes[..n].to_vec();
        let colors: Vec<[u8; 4]> = self.colors[..n].to_vec();
        let scores: Vec<f64> = self.scores[..n].to_vec();
        // The target is unchanged, so only the canvas needs uploading,
        // which load_shapes does.
        self.reset(None);
        self.load_shapes(&shapes, &colors, &scores);
    }

    /// Redraw the stored shapes on the background color with a long side of
    /// `size` pixels.
    pub fn render_at(&self, size: u32) -> Pixmap {
        let (sw, sh, scale) = output_size(self.w, self.h, size);
        let mut pm = blank_canvas(sw.max(1), sh.max(1), self.bg);
        draw_shapes(&mut pm, &self.shapes, &self.colors, scale);
        pm
    }

    /// GPU-drawing path: make several batches in one go and join them into
    /// one batch for a single GPU job (see `make_cands`). Each batch gets its
    /// own seed.
    fn prepare_cands<F>(&mut self, counts: &[usize], gen: F) -> Batch
    where
        F: Fn(usize, usize, &mut Rng) -> State + Sync,
    {
        let t0 = Instant::now();
        let seeds: Vec<u64> = counts.iter().map(|_| self.rng.next_u64()).collect();
        // A small batch is done sooner on this thread than the other threads
        // can be woken up.
        let serial = self.joined && counts.iter().sum::<usize>() <= SERIAL_MAX;
        let (b, cpu) = make_cands(self.w, self.h, counts, &seeds, serial, gen);
        let pt = self.phase_times();
        pt.make += cpu;
        pt.prepare += t0.elapsed().as_secs_f64();
        b
    }

    /// Create `n` states with `gen(i, rng)` and get them ready for the GPU:
    /// rasterized into spans (CPU-drawing path) or turned into shape records
    /// (GPU-drawing path). Runs in parallel on the CPU, in chunks that each
    /// get their own random generator.
    fn prepare<F>(&mut self, n: usize, gen: F) -> Batch
    where
        F: Fn(usize, &mut Rng) -> State + Sync,
    {
        if self.gpu_draw_now {
            return self.prepare_cands(&[n], |_, i, rng| gen(i, rng));
        }
        // Enough pieces that every CPU thread gets several.
        let chunk = (n / (rayon::current_num_threads() * 4)).clamp(1, 64);
        let (w, h) = (self.w, self.h);
        let seed = self.rng.next_u64();
        let t0 = Instant::now();
        let parts: Vec<Part> = (0..n.div_ceil(chunk))
            .into_par_iter()
            .map_init(Scratch::default, |sc, c| {
                let mut rng = Rng::new(seed ^ (c as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15));
                let range = c * chunk..((c + 1) * chunk).min(n);
                let mut p = Part { states: Vec::with_capacity(range.len()), ..Default::default() };
                for i in range {
                    let ta = Instant::now();
                    let st = gen(i, &mut rng);
                    let tb = Instant::now();
                    p.tm[0] += (tb - ta).as_secs_f64();
                    st.shape.rasterize(w, h, sc);
                    let tc = Instant::now();
                    p.tm[1] += (tc - tb).as_secs_f64();
                    let off = p.spans.len() as u32;
                    let mut start = 0u32;
                    for l in sc.lines.iter().filter(|l| l.x2 >= l.x1) {
                        p.spans.push(GpuSpan { y: l.y as u32, x1: l.x1 as u32, alpha: l.alpha, start });
                        start += (l.x2 - l.x1 + 1) as u32;
                    }
                    p.descs.push(GpuDesc {
                        span_offset: off,
                        span_count: p.spans.len() as u32 - off,
                        pixel_count: start,
                        alpha: st.alpha as u32,
                    });
                    p.states.push(st);
                    p.tm[2] += tc.elapsed().as_secs_f64();
                }
                p
            })
            .collect();
        let mut b = Batch {
            states: Vec::with_capacity(n),
            span_chunks: Vec::with_capacity(parts.len()),
            descs: Vec::with_capacity(n),
            cands: Vec::new(),
            parts: vec![n],
        };
        let mut base = 0u32;
        let pt = self.phase_times();
        for p in parts {
            pt.make += p.tm[0];
            pt.draw += p.tm[1];
            pt.pack += p.tm[2];
            b.states.extend_from_slice(&p.states);
            b.descs.extend(p.descs.into_iter().map(|mut d| {
                d.span_offset += base;
                d
            }));
            base += p.spans.len() as u32;
            b.span_chunks.push(p.spans);
        }
        pt.prepare += t0.elapsed().as_secs_f64();
        b
    }

    fn phase_times(&mut self) -> &mut PhaseTimes {
        if self.climbing {
            &mut self.times.climb
        } else {
            &mut self.times.random
        }
    }

    /// Send a prepared batch to a GPU slot. Returns its states.
    fn submit(&mut self, slot: usize, b: Batch) -> Vec<State> {
        if self.cfg.check {
            self.check_pending[slot] = Some((self.exact_deltas(&b.states), b.parts.clone()));
        }
        let t0 = Instant::now();
        let bytes = if b.cands.is_empty() {
            self.gpu.submit(slot, &b.span_chunks, &b.descs);
            let spans: usize = b.span_chunks.iter().map(|c| c.len()).sum();
            spans * std::mem::size_of::<GpuSpan>() + b.descs.len() * std::mem::size_of::<GpuDesc>()
        } else {
            self.gpu.submit_cands(slot, &b.cands);
            std::mem::size_of_val(b.cands.as_slice())
        };
        let pt = self.phase_times();
        pt.upload += t0.elapsed().as_secs_f64();
        pt.candidates += b.states.len();
        pt.batches += 1;
        pt.bytes += bytes as u64;
        self.counter += b.states.len();
        b.states
    }

    /// Exact CPU error change for each state (for `-check`).
    fn exact_deltas(&self, states: &[State]) -> Vec<i64> {
        let (w, h) = (self.w, self.h);
        let (target, current) = (&self.target, &self.current);
        let m = self.cfg.metric;
        states
            .par_iter()
            .map_init(Scratch::default, |sc, st| {
                st.shape.rasterize(w, h, sc);
                delta_lines(m, target, current, w, &sc.lines, st.alpha)
            })
            .collect()
    }

    /// Wait for a GPU slot; return the score each of its states would give.
    fn collect(&mut self, slot: usize) -> Vec<f64> {
        let (w, h) = (self.w, self.h);
        let total = self.total as i64;
        let m = self.cfg.metric;
        let t0 = Instant::now();
        let res = self.gpu.collect(slot);
        self.phase_times().wait += t0.elapsed().as_secs_f64();
        if let Some((exact, parts)) = self.check_pending[slot].take() {
            let gpu: Vec<i64> = res.iter().map(|r| r.delta()).collect();
            let mut at = 0;
            for n in parts {
                self.check.add(&gpu[at..at + n], &exact[at..at + n]);
                at += n;
            }
        }
        res.iter().map(|r| score_of(m, (total + r.delta()).max(0) as u64, w, h)).collect()
    }

    /// Score `n` random states, split into a few batches so the CPU can
    /// prepare one while the GPU scores another.
    fn score_random(&mut self, n: usize, shape_type: i32, alpha: i32) -> (Vec<State>, Vec<f64>) {
        let (w, h) = (self.w, self.h);
        let parts = n.div_ceil(2000).clamp(1, 8);
        let per = n.div_ceil(parts);
        let mut states = Vec::with_capacity(n);
        let mut energies = Vec::with_capacity(n);
        if self.joined {
            let t0 = Instant::now();
            let key: RandomKey = (shape_type, alpha, n);
            let seed_count = random_counts(n).len();
            // Use the shapes made in the background during the last step,
            // if they are of the kind this step needs.
            let (jobs, cpu) = match self.pregen.take() {
                Some((k, rx)) if k == key => rx.recv().expect("background shape maker stopped"),
                _ => {
                    let seeds: Vec<u64> = (0..seed_count).map(|_| self.rng.next_u64()).collect();
                    random_jobs(w, h, key, &seeds)
                }
            };
            // Start on the random shapes of the next step now, on the other
            // CPU threads, on the guess that it asks for the same kind. They
            // are ready long before this step's hill climb ends.
            let seeds: Vec<u64> = (0..seed_count).map(|_| self.rng.next_u64()).collect();
            let (tx, rx) = mpsc::sync_channel(1);
            rayon::spawn(move || {
                let _ = tx.send(random_jobs(w, h, key, &seeds));
            });
            self.pregen = Some((key, rx));
            let pt = self.phase_times();
            pt.make += cpu;
            pt.prepare += t0.elapsed().as_secs_f64();
            let slots = jobs.len();
            for (slot, b) in jobs.into_iter().enumerate() {
                states.extend(self.submit(slot, b));
            }
            for slot in 0..slots {
                energies.extend(self.collect(slot));
            }
            return (states, energies);
        }
        let mut inflight: VecDeque<(usize, Vec<State>)> = VecDeque::new();
        for p in 0..parts {
            let cnt = per.min(n - (p * per).min(n));
            if cnt == 0 {
                break;
            }
            let b = self.prepare(cnt, |_, rng| State::new(Shape::random(shape_type, w, h, rng), alpha));
            if inflight.len() == SLOTS {
                let (slot, st) = inflight.pop_front().unwrap();
                states.extend(st);
                energies.extend(self.collect(slot));
            }
            let slot = p % SLOTS;
            let st = self.submit(slot, b);
            inflight.push_back((slot, st));
        }
        while let Some((slot, st)) = inflight.pop_front() {
            states.extend(st);
            energies.extend(self.collect(slot));
        }
        (states, energies)
    }

    /// Start the next hill-climb round for chain group `g` in GPU slot `g`.
    fn climb_round(&mut self, chains: &[Chain], g: usize, groups: usize) -> Option<(Vec<usize>, Vec<State>)> {
        let (w, h, k, max_age) = (self.w, self.h, self.cfg.k.max(1), self.cfg.age);
        let live: Vec<usize> = (g..chains.len()).step_by(groups).filter(|&i| chains[i].age < max_age).collect();
        if live.is_empty() {
            return None;
        }
        let parents: Vec<State> = live.iter().map(|&i| chains[i].state).collect();
        let b = self.prepare(live.len() * k, |j, rng| {
            let mut s = parents[j / k];
            s.do_move(w, h, rng);
            s
        });
        let states = self.submit(g, b);
        Some((live, states))
    }

    /// Hill climb on the GPU-drawing path. The chains are split into the
    /// same groups, with the same seeds, as in `climb`, so the result is the
    /// same; but each round sends all groups to the GPU as one job, because
    /// the fixed cost of a GPU job is far larger than the work in it.
    fn climb_joined(&mut self, chains: &mut [Chain]) {
        let (w, h, k, max_age) = (self.w, self.h, self.cfg.k.max(1), self.cfg.age);
        let groups = SLOTS.min(chains.len());
        loop {
            let live: Vec<Vec<usize>> = (0..groups)
                .map(|g| (g..chains.len()).step_by(groups).filter(|&i| chains[i].age < max_age).collect())
                .filter(|l: &Vec<usize>| !l.is_empty())
                .collect();
            if live.is_empty() {
                break;
            }
            let parents: Vec<Vec<State>> = live.iter().map(|l| l.iter().map(|&i| chains[i].state).collect()).collect();
            let counts: Vec<usize> = live.iter().map(|l| l.len() * k).collect();
            let b = self.prepare_cands(&counts, |g, j, rng| {
                let mut s = parents[g][j / k];
                s.do_move(w, h, rng);
                s
            });
            let batch = self.submit(0, b);
            let e = self.collect(0);
            for (j, &i) in live.iter().flatten().enumerate() {
                let (bi, be) = e[j * k..(j + 1) * k]
                    .iter()
                    .enumerate()
                    .fold((0, f64::MAX), |acc, (n, &v)| if v < acc.1 { (n, v) } else { acc });
                if be < chains[i].energy {
                    chains[i].state = batch[j * k + bi];
                    chains[i].energy = be;
                    chains[i].age = 0;
                } else {
                    chains[i].age += k;
                }
            }
        }
    }

    /// Hill climb all chains. Chains are split into one group per GPU slot,
    /// so one group's next round is prepared while the other is scored.
    fn climb(&mut self, chains: &mut [Chain]) {
        if self.joined {
            return self.climb_joined(chains);
        }
        let k = self.cfg.k.max(1);
        let groups = SLOTS.min(chains.len());
        let mut pending: Vec<Option<(Vec<usize>, Vec<State>)>> = (0..groups).map(|_| None).collect();
        for g in 0..groups {
            pending[g] = self.climb_round(chains, g, groups);
        }
        loop {
            let mut any = false;
            for g in 0..groups {
                let Some((live, batch)) = pending[g].take() else {
                    continue;
                };
                any = true;
                let e = self.collect(g);
                for (j, &i) in live.iter().enumerate() {
                    let (bi, be) = e[j * k..(j + 1) * k]
                        .iter()
                        .enumerate()
                        .fold((0, f64::MAX), |acc, (n, &v)| if v < acc.1 { (n, v) } else { acc });
                    if be < chains[i].energy {
                        chains[i].state = batch[j * k + bi];
                        chains[i].energy = be;
                        chains[i].age = 0;
                    } else {
                        chains[i].age += k;
                    }
                }
                pending[g] = self.climb_round(chains, g, groups);
            }
            if !any {
                break;
            }
        }
    }

    /// Find and add one shape (plus `repeat` extra ones). Returns the number
    /// of candidates scored.
    pub fn step(&mut self, shape_type: i32, alpha: i32, repeat: usize) -> usize {
        self.counter = 0;
        self.gpu_draw_now = self.cfg.gpu_draw && Shape::gpu_supported(shape_type);
        // Curves (mode 6) cost much more CPU time per candidate than the
        // other kinds, so they keep the older scheme: every batch is prepared
        // on all threads while the GPU scores the batch before it.
        self.joined = self.gpu_draw_now && shape_type != 6;
        let (m, n) = (self.cfg.chains.max(1), self.cfg.random.max(1));
        self.climbing = false;
        let t0 = Instant::now();
        let (states, e) = self.score_random(m * n, shape_type, alpha);
        self.times.random.total += t0.elapsed().as_secs_f64();
        let mut chains: Vec<Chain> = (0..m)
            .map(|c| {
                let (bi, be) = e[c * n..(c + 1) * n]
                    .iter()
                    .enumerate()
                    .fold((0, f64::MAX), |acc, (i, &v)| if v < acc.1 { (i, v) } else { acc });
                Chain { state: states[c * n + bi], energy: be, age: 0 }
            })
            .collect();
        self.climbing = true;
        let t0 = Instant::now();
        self.climb(&mut chains);
        self.times.climb.total += t0.elapsed().as_secs_f64();
        let best = chains
            .iter()
            .min_by(|a, b| a.energy.total_cmp(&b.energy))
            .map(|c| c.state)
            .unwrap();
        self.add(best);

        let mut state = best;
        for _ in 0..repeat {
            let t0 = Instant::now();
            let b = self.prepare(1, |_, _| state);
            self.submit(0, b);
            let energy = self.collect(0)[0];
            let mut one = [Chain { state, energy, age: 0 }];
            self.climb(&mut one);
            self.times.climb.total += t0.elapsed().as_secs_f64();
            if one[0].energy >= self.score {
                break;
            }
            state = one[0].state;
            self.add(state);
        }
        self.counter
    }

    /// Commit a shape: exact CPU color and error, then update the GPU copy.
    pub fn add(&mut self, st: State) {
        let t0 = Instant::now();
        self.add_inner(st);
        self.times.commit += t0.elapsed().as_secs_f64();
    }

    fn add_inner(&mut self, st: State) {
        let (w, h) = (self.w, self.h);
        st.shape.rasterize(w, h, &mut self.scratch);
        let lines = &self.scratch.lines;
        let color = compute_color(&self.target, &self.current, w, lines, st.alpha);
        let before = self.current.clone();
        draw_lines(&mut self.current, w, color, lines);
        self.total = difference_partial(self.cfg.metric, &self.target, &before, &self.current, w, self.total, lines);
        self.score = score_of(self.cfg.metric, self.total, w, h);
        self.shapes.push(st.shape);
        self.colors.push(color);
        self.scores.push(self.score);
        self.gpu.set_current(&self.current);
        let t = self.base_transform();
        st.shape.draw(&mut self.canvas, &Self::paint(color), t);
    }

    pub fn svg(&self) -> String {
        svg_of(self.sw, self.sh, self.scale, self.bg, &self.shapes, &self.colors)
    }

    /// Output-size frames, one each time the score improved by `score_delta`.
    pub fn frames(&self, score_delta: f64) -> Vec<Pixmap> {
        let mut pm = Self::new_canvas(self.canvas.width(), self.canvas.height(), self.bg);
        let mut out = vec![pm.clone()];
        let t = self.base_transform();
        let mut previous = 10.0;
        for (i, s) in self.shapes.iter().enumerate() {
            s.draw(&mut pm, &Self::paint(self.colors[i]), t);
            let score = self.scores[i];
            if previous - score >= score_delta {
                previous = score;
                out.push(pm.clone());
            }
        }
        out
    }
}
