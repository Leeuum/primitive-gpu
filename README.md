# Primitive Pictures, on the GPU

Rebuild any picture from simple shapes (triangles, rectangles, circles, curves and more), one shape at a time.

A Rust port of Michael Fogleman's [primitive](https://github.com/fogleman/primitive) that scores millions of candidate shapes per second on the graphics card with [wgpu](https://wgpu.rs). It has two programs: the command-line `primitive-gpu`, with the original's flags plus a few new ones, and `primitive-gui`, a browser interface for pictures, videos and image sequences.

![The browser GUI building a video frame by frame with rotated rectangles](docs/images/gui.jpg)

## Download

No install needed. Get the file for your computer from the [Releases page](https://github.com/Leeuum/primitive-gpu/releases/latest) (Windows, Mac with Apple Silicon, or Linux), unzip it, and double-click `primitive-gui`. The GUI opens in your browser. Close the small text window to stop it. ffmpeg comes included. The download has only the GUI; to use the command-line program, build it yourself (see [Building](#building)).

The first time, Windows and Mac warn you because the program is not signed:

- **Windows:** click "More info", then "Run anyway".
- **Mac:** click "Done", then open System Settings > Privacy & Security and click "Open Anyway".

`START-HERE.txt` in the download explains the rest.

## How it works

The canvas starts as the average colour of the target. The program finds the shape that brings the canvas closest to the target, adds it, and repeats. To find each shape:

1. **Random phase.** Many random shapes are scored. The best few become starting points.
2. **Climb phase.** Each starting point is nudged again and again (move a corner, rotate), keeping each nudge that improves the score.
3. **Commit.** The best shape is drawn onto the canvas.

Each shape's colour is worked out directly from the pixels under it. The score is the root-mean-square difference from the target (lower is closer).

The GPU only *ranks* candidates; the winner is drawn again on the CPU with exact whole-number maths. Both sides use the same whole-number maths, so the GPU picks the same winners as an exact CPU search (`rust/check.sh` tests this). Curves, rotated ellipses and polygons have anti-aliased edges that differ slightly, picking a different winner in about 1% of batches.

## Speed

Average time for 50 shapes at a 256-pixel working size (Ryzen 7 2700, RTX 2060):

| Shape | combo | triangle | rect | ellipse | circle | rotated rect | curve | rotated ellipse | polygon |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Seconds | 0.57 | 0.36 | 0.18 | 0.26 | 0.23 | 0.23 | 1.01 | 0.72 | 0.64 |

## Building

To build it yourself, you need [Rust](https://rustup.rs), a graphics card with Vulkan, Metal or DirectX 12, and [ffmpeg](https://ffmpeg.org) for videos and formats such as HEIC.

```bash
cd rust
cargo build --release
```

The programs land in `rust/target/release/`. They use an `ffmpeg` placed next to them first, then the one on the system path.

## Command-line use

The command-line program is not in the download. Build it as shown above.

```bash
cd rust
target/release/primitive-gpu -i ../examples/monalisa.png -o out.png -n 100 -m 1 -v
```

| Flag | Default | Description |
| --- | --- | --- |
| `i` | n/a | input image (`-` reads standard input) |
| `o` | n/a | output file: `.png`, `.jpg`, `.svg` or `.gif` (repeatable; `-` writes SVG to standard output) |
| `n` | n/a | number of shapes (repeatable; each uses the `-m`, `-a` and `-rep` before it) |
| `m` | 1 | mode: 0=combo, 1=triangle, 2=rect, 3=ellipse, 4=circle, 5=rotatedrect, 6=beziers, 7=rotatedellipse, 8=polygon |
| `a` | 128 | shape alpha (0 lets the search choose) |
| `bg` | average | starting background colour (hex) |
| `r` | 256 | working size for the search |
| `s` | 1024 | output image size |
| `j` | all cores | number of CPU threads |
| `nth` | 1 | save every Nth frame (with `%d` in the output path) |
| `rep` | 0 | extra shapes per step with a smaller search (good for curves) |
| `metric` | squared | `squared` or `plain` (flatter, bolder shapes) |
| `score` | off | stop once the score is at or below this, e.g. 0.03 |
| `v`, `vv` | off | verbose output (`-vv` adds a timing table) |

GPU search tuning:

| Flag | Default | Description |
| --- | --- | --- |
| `chains` | 16 | hill-climbing chains per shape |
| `random` | 1000 | random tries that seed each chain |
| `age` | 100 | tries without improvement before a chain stops |
| `k` | 32 | nudges each chain tries per GPU round |
| `seed` | clock | random seed; repeatable with the same `-j` |
| `raster` | gpu | who draws the candidate shapes: `gpu` or `cpu` (the older path) |
| `check` | off | also score every candidate exactly on the CPU and report the GPU's error (slow) |

GIF output animates the shapes being added. With `%d` in a PNG, JPG or SVG name, every frame is saved as its own file.

## Browser GUI

```bash
cd rust
target/release/primitive-gui            # opens http://localhost:8640 in the browser
target/release/primitive-gui -port 9000 -j 8 -no-open
```

The page works from any device on the same network.

- **Input:** an image, a video, or a folder of pictures (an image sequence).
- **Stopping:** "Shapes" sets the most shapes per picture, "Stop at score" stops once it is close enough, and "Fewest shapes" keeps dark frames from stopping too soon.
- **Export:** PNG, JPG, SVG or a timelapse, at any size up to 4K without a new search.
- **Videos:** every loop adds more shapes to each frame. Each frame can start fresh or build on the one before.
- **Render:** saves an MP4 or WebM (with the original sound), a GIF, or an image sequence to `primitive` in your Videos folder (Movies on a Mac). Existing files are never overwritten.

**Security:** the GUI has no password and listens on all network interfaces. Never expose its port to the internet. To report a security problem, see [SECURITY.md](SECURITY.md).

## Testing and adding shapes

```bash
cd rust
./check.sh                  # GPU vs exact CPU scores for every mode: PASS or FAIL
./bench.sh > results.tsv    # speed: one row per mode, seed and picture
```

To add a shape, add a variant to the `Shape` enum in `rust/src/shapes.rs`, handle it in each of its methods, add a matching `walk_*` function to `rust/src/raster_score.wgsl`, and run `./check.sh`.

## Maintainer

Maintained by [Leeuum](https://github.com/Leeuum) in spare time, so replies can be slow. Bug reports and ideas are welcome as [issues](https://github.com/Leeuum/primitive-gpu/issues).

## Credits

- **Michael Fogleman** created the original [primitive](https://github.com/fogleman/primitive) algorithm and Go program (MIT licence). This port follows its design, flags and shape modes.
- The example pictures are public domain or CC0. See [examples/CREDITS.md](examples/CREDITS.md).
- Written with **Claude**, using [Claude Code](https://claude.com/claude-code).

## Licence

MIT. See [LICENSE](LICENSE).
