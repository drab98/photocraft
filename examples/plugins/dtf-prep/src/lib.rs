//! DTF Prep: hard-alpha halftoning for Direct-to-Film artwork (PhotoCraft plug-in ABI v1).
//!
//! The run-time path is `no_std`, allocation-free, deterministic, and uses only the pixel buffer
//! plus the host-provided JSON parameters. See `docs/plugins.md` for the ABI contract.
#![cfg_attr(all(not(test), target_arch = "wasm32"), no_std)]
#![cfg_attr(all(not(test), not(target_arch = "wasm32")), allow(dead_code))]

const MANIFEST: &str = r#"{
  "id": "cz.optimaprint.dtfprep",
  "name": "DTF Prep",
  "version": "0.1.0",
  "kind": "filter",
  "author": "PhotoCraft contributors",
  "description": "Prepares transparent RGBA artwork for DTF printing using hard-alpha halftoning.",
  "params": {
    "alphaThreshold": {"type":"number", "min":0, "max":255, "default":8},
    "processingMode": {"type":"choice", "options":["softTransparencyOnly","fullArtworkForcedHalftone"], "default":"softTransparencyOnly"},
    "pattern": {"type":"choice", "options":["roundDots","lines"], "default":"roundDots"},
    "cellSize": {"type":"number", "min":2, "max":64, "default":8},
    "angle": {"type":"number", "min":0, "max":180, "default":22.5},
    "ventilationPercent": {"type":"number", "min":0, "max":80, "default":20},
    "knockout": {"type":"choice", "options":["off","black","white","custom"], "default":"off"},
    "customR": {"type":"number", "min":0, "max":255, "default":0},
    "customG": {"type":"number", "min":0, "max":255, "default":0},
    "customB": {"type":"number", "min":0, "max":255, "default":0},
    "knockoutTolerance": {"type":"number", "min":0, "max":255, "default":24},
    "shadowColorBoost": {"type":"number", "min":0, "max":100, "default":0},
    "speckCleanup": {"type":"choice", "options":["off","light","medium","strong"], "default":"light"}
  },
  "overlap": 2,
  "area": "content"
}"#;

const HAS_ALPHA: u32 = 1 << 8;
const RGB_MODE: u32 = 3;
const OVERLAP: f32 = 2.0;
/// Area of a centred circle intersected with a unit square, sampled at radius² = i / 64.
/// Looking up this radial CDF makes round-dot coverage track the requested alpha all the way from
/// zero to one while avoiding transcendental math for every interpreted pixel.
#[allow(clippy::approx_constant)]
const ROUND_CDF: [f32; 33] = [
    0.0,
    0.049_087_39,
    0.098_174_77,
    0.147_262_16,
    0.196_349_55,
    0.245_436_92,
    0.294_524_3,
    0.343_611_7,
    0.392_699_1,
    0.441_786_47,
    0.490_873_84,
    0.539_961_2,
    0.589_048_6,
    0.638_136,
    0.687_223_4,
    0.736_310_8,
    0.785_398_2,
    0.824_195_74,
    0.854_809_8,
    0.880_415_56,
    0.902_188_2,
    0.920_818_75,
    0.936_777_5,
    0.950_410_2,
    0.961_984_46,
    0.971_714_14,
    0.979_774_83,
    0.986_313_34,
    0.991_454_54,
    0.995_305_84,
    0.997_960_75,
    0.999_501_3,
    1.0,
];

const ERR_BUFFER: i32 = 1;
const ERR_UNSUPPORTED_FORMAT: i32 = 2;
const ERR_PARAMS: i32 = 3;

/// Temporary alpha markers. Zero is a preliminary hole, one is a screened pixel, two is a
/// cleanup survivor, three is a protected opaque pixel, and negative one is a cleanup removal.
const SCREENED: f32 = 1.0;
const CLEANED: f32 = 2.0;
const PROTECTED: f32 = 3.0;
const REMOVED: f32 = -1.0;

#[cfg(all(not(test), target_arch = "wasm32"))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProcessingMode {
    SoftTransparencyOnly,
    FullArtwork,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pattern {
    RoundDots,
    Lines,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Knockout {
    Off,
    Black,
    White,
    Custom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cleanup {
    Off,
    Light,
    Medium,
    Strong,
}

#[derive(Clone, Copy, Debug)]
struct Params {
    alpha_threshold: f32,
    processing_mode: ProcessingMode,
    pattern: Pattern,
    cell_size: f32,
    angle: f32,
    ventilation: f32,
    knockout: Knockout,
    custom: [f32; 3],
    knockout_tolerance: f32,
    shadow_boost: f32,
    cleanup: Cleanup,
    origin_x: i32,
    origin_y: i32,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            alpha_threshold: 8.0,
            processing_mode: ProcessingMode::SoftTransparencyOnly,
            pattern: Pattern::RoundDots,
            cell_size: 8.0,
            angle: 22.5,
            ventilation: 20.0,
            knockout: Knockout::Off,
            custom: [0.0, 0.0, 0.0],
            knockout_tolerance: 24.0,
            shadow_boost: 0.0,
            cleanup: Cleanup::Light,
            origin_x: 0,
            origin_y: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct Screen {
    pattern: Pattern,
    cosine: f32,
    sine: f32,
    inverse_cell: f32,
}

impl Screen {
    fn new(pattern: Pattern, angle: f32, cell_size: f32) -> Self {
        let (sine, cosine) = sin_cos_degrees(angle);
        Self { pattern, cosine, sine, inverse_cell: 1.0 / cell_size }
    }

    fn keeps(self, x: i32, y: i32, coverage: f32) -> bool {
        if coverage <= 0.0 {
            return false;
        }
        if coverage >= 1.0 {
            return true;
        }
        let x = x as f32 + 0.5;
        let y = y as f32 + 0.5;
        let u = (x * self.cosine + y * self.sine) * self.inverse_cell;
        let du = fraction(u) - 0.5;
        let threshold = match self.pattern {
            Pattern::Lines => 2.0 * absolute(du),
            Pattern::RoundDots => {
                let v = (-x * self.sine + y * self.cosine) * self.inverse_cell;
                let dv = fraction(v) - 0.5;
                round_coverage_rank(du * du + dv * dv)
            }
        };
        coverage >= threshold
    }
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn pc_abi_version() -> u32 {
    1
}

/// `len << 32 | ptr` of the UTF-8 manifest JSON.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn pc_manifest() -> u64 {
    ((MANIFEST.len() as u64) << 32) | MANIFEST.as_ptr() as u64
}

/// Fresh pages for the host-owned input block. Every band gets a new instance, so no free is
/// required by ABI v1.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn pc_alloc(size: u32) -> u32 {
    let pages = (size as usize).div_ceil(65_536);
    let old = core::arch::wasm32::memory_grow(0, pages);
    if old == usize::MAX { 0 } else { (old * 65_536) as u32 }
}

/// Filters one padded PhotoCraft band in place.
///
/// # Safety
/// PhotoCraft guarantees that both pointer/length pairs address blocks returned by `pc_alloc` in
/// this module's memory. Lengths and the buffer geometry are checked before either slice is used.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_filter(buf: *mut f32, len: u32, width: u32, height: u32, channels: u32, format: u32, params: *const u8, params_len: u32) -> i32 {
    if buf.is_null() || params.is_null() || params_len == 0 || params_len > 65_536 {
        return ERR_BUFFER;
    }
    let Some(samples) = (len as usize).checked_div(4) else { return ERR_BUFFER };
    if len % 4 != 0 {
        return ERR_BUFFER;
    }
    // SAFETY: the pointer contract is stated above; `samples` is derived from its byte length.
    let pixels = unsafe { core::slice::from_raw_parts_mut(buf, samples) };
    // SAFETY: the pointer contract is stated above and the parameter length is capped above.
    let json = unsafe { core::slice::from_raw_parts(params, params_len as usize) };
    filter_impl(pixels, width, height, channels, format, json)
}

fn filter_impl(pixels: &mut [f32], width: u32, height: u32, channels: u32, format: u32, json: &[u8]) -> i32 {
    if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
        return ERR_BUFFER;
    }
    let expected = width.checked_mul(height).and_then(|n| n.checked_mul(channels)).and_then(|n| usize::try_from(n).ok());
    if expected != Some(pixels.len()) {
        return ERR_BUFFER;
    }
    let depth = format & 0xff;
    let mode = (format >> 16) & 0xff;
    if channels != 4 || format & HAS_ALPHA == 0 || mode != RGB_MODE || !matches!(depth, 8 | 16 | 32) {
        return ERR_UNSUPPORTED_FORMAT;
    }
    let Some(params) = parse_params(json, width, height, channels, format) else { return ERR_PARAMS };
    process_pixels(pixels, width as usize, height as usize, params);
    0
}

fn process_pixels(pixels: &mut [f32], width: usize, height: usize, params: Params) {
    let screen = Screen::new(params.pattern, params.angle, params.cell_size);
    let alpha_cutoff = params.alpha_threshold / 255.0;
    let maximum_coverage = 1.0 - params.ventilation / 100.0;

    // Pass one creates the deterministic binary screen. Protected opaque pixels use a distinct
    // marker so cleanup cannot remove small lettering or details in Soft Transparency Only mode.
    for (i, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let [r, g, b, a] = pixel;
        let source_alpha = finite_unit(*a);
        if source_alpha <= 0.0 || source_alpha < alpha_cutoff {
            clear_components(r, g, b, a);
            continue;
        }

        let knockout = knockout_factor(*r, *g, *b, params);
        let mut coverage = source_alpha;
        if params.processing_mode == ProcessingMode::FullArtwork {
            coverage = if coverage < maximum_coverage { coverage } else { maximum_coverage };
        }
        coverage *= knockout;
        coverage = finite_unit(coverage);
        let protected = params.processing_mode == ProcessingMode::SoftTransparencyOnly && source_alpha >= 1.0 && knockout >= 1.0;

        let row = i / width;
        let col = i % width;
        let gx = params.origin_x.saturating_add(col as i32);
        let gy = params.origin_y.saturating_add(row as i32);
        if protected || screen.keeps(gx, gy, coverage) {
            if params.shadow_boost > 0.0 {
                boost_shadow_color(r, g, b, params.shadow_boost);
            }
            *a = if protected { PROTECTED } else { SCREENED };
        } else {
            clear_components(r, g, b, a);
        }
    }

    cleanup_in_place(pixels, width, height, params.cleanup);
    normalize_alpha(pixels);
}

fn knockout_factor(r: f32, g: f32, b: f32, params: Params) -> f32 {
    let target = match params.knockout {
        Knockout::Off => return 1.0,
        Knockout::Black => [0.0, 0.0, 0.0],
        Knockout::White => [1.0, 1.0, 1.0],
        Knockout::Custom => params.custom,
    };
    let distance = maximum3(absolute(finite_unit(r) - target[0]), absolute(finite_unit(g) - target[1]), absolute(finite_unit(b) - target[2]));
    let tolerance = params.knockout_tolerance / 255.0;
    if tolerance <= 0.0 {
        return if distance <= f32::EPSILON { 0.0 } else { 1.0 };
    }
    let t = finite_unit(distance / tolerance);
    // Smoothly hand coverage to the screen near the tolerance edge instead of leaving a hard halo.
    t * t * (3.0 - 2.0 * t)
}

fn boost_shadow_color(r: &mut f32, g: &mut f32, b: &mut f32, amount: f32) {
    let (rr, gg, bb) = (finite_unit(*r), finite_unit(*g), finite_unit(*b));
    let luma = 0.2126 * rr + 0.7152 * gg + 0.0722 * bb;
    let strength = (amount / 100.0) * 0.35 * (1.0 - luma);
    *r = finite_unit(luma + (rr - luma) * (1.0 + strength));
    *g = finite_unit(luma + (gg - luma) * (1.0 + strength));
    *b = finite_unit(luma + (bb - luma) * (1.0 + strength));
}

fn cleanup_in_place(pixels: &mut [f32], width: usize, height: usize, level: Cleanup) {
    let Some((radius, minimum)) = cleanup_rule(level) else { return };
    if width == 0 || height == 0 {
        return;
    }
    for row in 0..height {
        for col in 0..width {
            let Some(alpha_index) = pixel_alpha_index(width, row, col) else { continue };
            let state = pixels.get(alpha_index).copied().unwrap_or(0.0);
            if state == 0.0 || state == PROTECTED {
                continue;
            }
            let y0 = row.saturating_sub(radius);
            let y1 = row.saturating_add(radius).min(height - 1);
            let x0 = col.saturating_sub(radius);
            let x1 = col.saturating_add(radius).min(width - 1);
            let mut neighbours = 0usize;
            'neighbours: for y in y0..=y1 {
                for x in x0..=x1 {
                    let Some(i) = pixel_alpha_index(width, y, x) else { continue };
                    // CLEANED and REMOVED both count their original preliminary state. This keeps
                    // the decision independent of scan order without allocating a second mask.
                    if pixels.get(i).copied().unwrap_or(0.0) != 0.0 {
                        neighbours += 1;
                        if neighbours >= minimum {
                            break 'neighbours;
                        }
                    }
                }
            }
            if let Some(alpha) = pixels.get_mut(alpha_index) {
                *alpha = if neighbours >= minimum { CLEANED } else { REMOVED };
            }
        }
    }
}

fn cleanup_rule(level: Cleanup) -> Option<(usize, usize)> {
    match level {
        Cleanup::Off => None,
        Cleanup::Light => Some((1, 2)),
        Cleanup::Medium => Some((1, 3)),
        Cleanup::Strong => Some((2, 5)),
    }
}

fn pixel_alpha_index(width: usize, row: usize, col: usize) -> Option<usize> {
    row.checked_mul(width)?.checked_add(col)?.checked_mul(4)?.checked_add(3)
}

fn normalize_alpha(pixels: &mut [f32]) {
    for pixel in pixels.as_chunks_mut::<4>().0 {
        let [r, g, b, a] = pixel;
        if *a > 0.0 {
            *a = 1.0;
        } else {
            clear_components(r, g, b, a);
        }
    }
}

fn clear_components(r: &mut f32, g: &mut f32, b: &mut f32, a: &mut f32) {
    *r = 0.0;
    *g = 0.0;
    *b = 0.0;
    *a = 0.0;
}

fn finite_unit(value: f32) -> f32 {
    if !value.is_finite() || value <= 0.0 {
        0.0
    } else if value >= 1.0 {
        1.0
    } else {
        value
    }
}

fn absolute(value: f32) -> f32 {
    if value < 0.0 { -value } else { value }
}

fn maximum3(a: f32, b: f32, c: f32) -> f32 {
    let ab = if a > b { a } else { b };
    if ab > c { ab } else { c }
}

/// Fractional part in 0..1 for the finite coordinate range supported by ABI v1 (`i32` origins).
fn fraction(value: f32) -> f32 {
    if !value.is_finite() {
        return 0.0;
    }
    let truncated = value as i32 as f32;
    let floor = if value < truncated { truncated - 1.0 } else { truncated };
    value - floor
}

fn round_coverage_rank(radius_squared: f32) -> f32 {
    if !radius_squared.is_finite() || radius_squared <= 0.0 {
        return 0.0;
    }
    if radius_squared >= 0.5 {
        return 1.0;
    }
    let scaled = radius_squared * 64.0;
    let index = scaled as usize;
    let blend = scaled - index as f32;
    let low = ROUND_CDF.get(index).copied().unwrap_or(1.0);
    let high = ROUND_CDF.get(index.saturating_add(1)).copied().unwrap_or(1.0);
    low + (high - low) * blend
}

/// Bhaskara I's sine approximation is deterministic, cheap in an interpreter, and exact at
/// 0/90/180 degrees. Its sub-percent error is immaterial for a pixel screen.
fn sin_cos_degrees(angle: f32) -> (f32, f32) {
    let degrees = if angle.is_finite() { angle.clamp(0.0, 180.0) } else { 0.0 };
    let sine = sin_zero_to_pi(degrees * core::f32::consts::PI / 180.0);
    let cosine = if degrees <= 90.0 {
        sin_zero_to_pi((90.0 - degrees) * core::f32::consts::PI / 180.0)
    } else {
        -sin_zero_to_pi((degrees - 90.0) * core::f32::consts::PI / 180.0)
    };
    (sine, cosine)
}

fn sin_zero_to_pi(x: f32) -> f32 {
    if x <= 0.0 || x >= core::f32::consts::PI {
        return 0.0;
    }
    let product = x * (core::f32::consts::PI - x);
    let denominator = 5.0 * core::f32::consts::PI * core::f32::consts::PI - 4.0 * product;
    if denominator <= 0.0 { 0.0 } else { 16.0 * product / denominator }
}

fn parse_params(json: &[u8], width: u32, height: u32, channels: u32, format: u32) -> Option<Params> {
    const REQUIRED: u32 = (1 << 14) - 1;
    let mut cursor = JsonCursor::new(json);
    let mut params = Params::default();
    let mut custom = [0.0f32; 3];
    let mut seen = 0u32;
    let mut image = None;

    cursor.byte(b'{')?;
    loop {
        cursor.space();
        if cursor.peek() == Some(b'}') {
            cursor.at += 1;
            break;
        }
        let key = cursor.string()?;
        cursor.byte(b':')?;
        let bit = match key {
            b"alphaThreshold" => 0,
            b"processingMode" => 1,
            b"pattern" => 2,
            b"cellSize" => 3,
            b"angle" => 4,
            b"ventilationPercent" => 5,
            b"knockout" => 6,
            b"customR" => 7,
            b"customG" => 8,
            b"customB" => 9,
            b"knockoutTolerance" => 10,
            b"shadowColorBoost" => 11,
            b"speckCleanup" => 12,
            b"_image" => 13,
            _ => {
                cursor.skip_value(1)?;
                cursor.object_separator()?;
                continue;
            }
        };
        let flag = 1u32.checked_shl(bit)?;
        if seen & flag != 0 {
            return None;
        }
        seen |= flag;
        match bit {
            0 => params.alpha_threshold = cursor.number()?,
            1 => {
                params.processing_mode = match cursor.string()? {
                    b"softTransparencyOnly" => ProcessingMode::SoftTransparencyOnly,
                    b"fullArtworkForcedHalftone" => ProcessingMode::FullArtwork,
                    _ => return None,
                };
            }
            2 => {
                params.pattern = match cursor.string()? {
                    b"roundDots" => Pattern::RoundDots,
                    b"lines" => Pattern::Lines,
                    _ => return None,
                };
            }
            3 => params.cell_size = cursor.number()?,
            4 => params.angle = cursor.number()?,
            5 => params.ventilation = cursor.number()?,
            6 => {
                params.knockout = match cursor.string()? {
                    b"off" => Knockout::Off,
                    b"black" => Knockout::Black,
                    b"white" => Knockout::White,
                    b"custom" => Knockout::Custom,
                    _ => return None,
                };
            }
            7 => custom[0] = cursor.number()?,
            8 => custom[1] = cursor.number()?,
            9 => custom[2] = cursor.number()?,
            10 => params.knockout_tolerance = cursor.number()?,
            11 => params.shadow_boost = cursor.number()?,
            12 => {
                params.cleanup = match cursor.string()? {
                    b"off" => Cleanup::Off,
                    b"light" => Cleanup::Light,
                    b"medium" => Cleanup::Medium,
                    b"strong" => Cleanup::Strong,
                    _ => return None,
                };
            }
            13 => image = Some(parse_image(&mut cursor)?),
            _ => return None,
        }
        cursor.object_separator()?;
    }
    cursor.space();
    if cursor.at != json.len() || seen != REQUIRED {
        return None;
    }
    let image = image?;
    let depth = format & 0xff;
    if image.width != width
        || image.height != height
        || image.channels != channels
        || image.depth != depth
        || image.overlap != OVERLAP as u32
        || !image.alpha
        || image.mode != ImageMode::Rgb
        || image.canvas_width == 0
        || image.canvas_height == 0
    {
        return None;
    }
    if !in_range(params.alpha_threshold, 0.0, 255.0)
        || !in_range(params.cell_size, 2.0, 64.0)
        || !in_range(params.angle, 0.0, 180.0)
        || !in_range(params.ventilation, 0.0, 80.0)
        || custom.iter().any(|value| !in_range(*value, 0.0, 255.0))
        || !in_range(params.knockout_tolerance, 0.0, 255.0)
        || !in_range(params.shadow_boost, 0.0, 100.0)
    {
        return None;
    }
    params.custom = [custom[0] / 255.0, custom[1] / 255.0, custom[2] / 255.0];
    params.origin_x = image.x;
    params.origin_y = image.y;
    Some(params)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageMode {
    Rgb,
}

#[derive(Clone, Copy, Debug)]
struct ImageContext {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    overlap: u32,
    canvas_width: u32,
    canvas_height: u32,
    mode: ImageMode,
    depth: u32,
    alpha: bool,
    channels: u32,
}

fn parse_image(cursor: &mut JsonCursor<'_>) -> Option<ImageContext> {
    const REQUIRED: u16 = (1 << 11) - 1;
    let mut image = ImageContext {
        x: 0,
        y: 0,
        width: 0,
        height: 0,
        overlap: 0,
        canvas_width: 0,
        canvas_height: 0,
        mode: ImageMode::Rgb,
        depth: 0,
        alpha: false,
        channels: 0,
    };
    let mut seen = 0u16;
    cursor.byte(b'{')?;
    loop {
        cursor.space();
        if cursor.peek() == Some(b'}') {
            cursor.at += 1;
            break;
        }
        let key = cursor.string()?;
        cursor.byte(b':')?;
        let bit = match key {
            b"x" => 0,
            b"y" => 1,
            b"width" => 2,
            b"height" => 3,
            b"overlap" => 4,
            b"canvasWidth" => 5,
            b"canvasHeight" => 6,
            b"mode" => 7,
            b"depth" => 8,
            b"alpha" => 9,
            b"channels" => 10,
            _ => {
                cursor.skip_value(2)?;
                cursor.object_separator()?;
                continue;
            }
        };
        let flag = 1u16.checked_shl(bit)?;
        if seen & flag != 0 {
            return None;
        }
        seen |= flag;
        match bit {
            0 => image.x = cursor.i32()?,
            1 => image.y = cursor.i32()?,
            2 => image.width = cursor.u32()?,
            3 => image.height = cursor.u32()?,
            4 => image.overlap = cursor.u32()?,
            5 => image.canvas_width = cursor.u32()?,
            6 => image.canvas_height = cursor.u32()?,
            7 => {
                image.mode = match cursor.string()? {
                    b"rgb" => ImageMode::Rgb,
                    _ => return None,
                };
            }
            8 => image.depth = cursor.u32()?,
            9 => image.alpha = cursor.boolean()?,
            10 => image.channels = cursor.u32()?,
            _ => return None,
        }
        cursor.object_separator()?;
    }
    if seen == REQUIRED { Some(image) } else { None }
}

fn in_range(value: f32, min: f32, max: f32) -> bool {
    value.is_finite() && value >= min && value <= max
}

struct JsonCursor<'a> {
    input: &'a [u8],
    at: usize,
}

impl<'a> JsonCursor<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self { input, at: 0 }
    }

    fn space(&mut self) {
        while self.input.get(self.at).is_some_and(|byte| byte.is_ascii_whitespace()) {
            self.at += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.space();
        self.input.get(self.at).copied()
    }

    fn byte(&mut self, expected: u8) -> Option<()> {
        self.space();
        if self.input.get(self.at) != Some(&expected) {
            return None;
        }
        self.at += 1;
        Some(())
    }

    fn string(&mut self) -> Option<&'a [u8]> {
        self.byte(b'"')?;
        let start = self.at;
        while let Some(byte) = self.input.get(self.at).copied() {
            match byte {
                b'"' => {
                    let value = self.input.get(start..self.at)?;
                    self.at += 1;
                    return Some(value);
                }
                b'\\' | 0..=0x1f => return None,
                _ => self.at += 1,
            }
        }
        None
    }

    fn boolean(&mut self) -> Option<bool> {
        self.space();
        if self.input.get(self.at..self.at.checked_add(4)?) == Some(b"true") {
            self.at += 4;
            Some(true)
        } else if self.input.get(self.at..self.at.checked_add(5)?) == Some(b"false") {
            self.at += 5;
            Some(false)
        } else {
            None
        }
    }

    fn i32(&mut self) -> Option<i32> {
        let value = self.number()?;
        if value < i32::MIN as f32 || value > i32::MAX as f32 {
            return None;
        }
        let integer = value as i32;
        if integer as f32 == value { Some(integer) } else { None }
    }

    fn u32(&mut self) -> Option<u32> {
        let value = self.number()?;
        if value < 0.0 || value > u32::MAX as f32 {
            return None;
        }
        let integer = value as u32;
        if integer as f32 == value { Some(integer) } else { None }
    }

    fn number(&mut self) -> Option<f32> {
        self.space();
        let mut sign = 1.0f32;
        if self.input.get(self.at) == Some(&b'-') {
            sign = -1.0;
            self.at += 1;
        }
        let first = self.input.get(self.at).copied()?;
        let mut value = 0.0f32;
        if first == b'0' {
            self.at += 1;
            if self.input.get(self.at).is_some_and(u8::is_ascii_digit) {
                return None;
            }
        } else if (b'1'..=b'9').contains(&first) {
            while let Some(byte) = self.input.get(self.at).copied().filter(u8::is_ascii_digit) {
                value = value * 10.0 + f32::from(byte - b'0');
                self.at += 1;
            }
        } else {
            return None;
        }
        if self.input.get(self.at) == Some(&b'.') {
            self.at += 1;
            let mut place = 0.1f32;
            let mut digits = 0usize;
            while let Some(byte) = self.input.get(self.at).copied().filter(u8::is_ascii_digit) {
                value += f32::from(byte - b'0') * place;
                place *= 0.1;
                self.at += 1;
                digits += 1;
            }
            if digits == 0 {
                return None;
            }
        }
        if self.input.get(self.at).is_some_and(|byte| *byte == b'e' || *byte == b'E') {
            self.at += 1;
            let mut exponent_sign = 1i32;
            if self.input.get(self.at) == Some(&b'-') {
                exponent_sign = -1;
                self.at += 1;
            } else if self.input.get(self.at) == Some(&b'+') {
                self.at += 1;
            }
            let mut exponent = 0i32;
            let mut digits = 0usize;
            while let Some(byte) = self.input.get(self.at).copied().filter(u8::is_ascii_digit) {
                exponent = exponent.saturating_mul(10).saturating_add(i32::from(byte - b'0'));
                self.at += 1;
                digits += 1;
            }
            if digits == 0 || exponent > 38 {
                return None;
            }
            let mut scale = 1.0f32;
            for _ in 0..exponent {
                scale *= 10.0;
            }
            value = if exponent_sign < 0 { value / scale } else { value * scale };
        }
        value *= sign;
        if value.is_finite() { Some(value) } else { None }
    }

    fn object_separator(&mut self) -> Option<()> {
        self.space();
        match self.input.get(self.at).copied()? {
            b',' => {
                self.at += 1;
                if self.peek() == Some(b'}') { None } else { Some(()) }
            }
            b'}' => Some(()),
            _ => None,
        }
    }

    fn skip_value(&mut self, depth: u8) -> Option<()> {
        if depth > 8 {
            return None;
        }
        self.space();
        match self.input.get(self.at).copied()? {
            b'"' => {
                self.string()?;
                Some(())
            }
            b'{' => self.skip_object(depth + 1),
            b'[' => self.skip_array(depth + 1),
            b't' | b'f' => self.boolean().map(|_| ()),
            b'n' if self.input.get(self.at..self.at.checked_add(4)?) == Some(b"null") => {
                self.at += 4;
                Some(())
            }
            b'-' | b'0'..=b'9' => self.number().map(|_| ()),
            _ => None,
        }
    }

    fn skip_object(&mut self, depth: u8) -> Option<()> {
        self.byte(b'{')?;
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Some(());
        }
        loop {
            self.string()?;
            self.byte(b':')?;
            self.skip_value(depth)?;
            self.space();
            match self.input.get(self.at).copied()? {
                b',' => self.at += 1,
                b'}' => {
                    self.at += 1;
                    return Some(());
                }
                _ => return None,
            }
        }
    }

    fn skip_array(&mut self, depth: u8) -> Option<()> {
        self.byte(b'[')?;
        if self.peek() == Some(b']') {
            self.at += 1;
            return Some(());
        }
        loop {
            self.skip_value(depth)?;
            self.space();
            match self.input.get(self.at).copied()? {
                b',' => self.at += 1,
                b']' => {
                    self.at += 1;
                    return Some(());
                }
                _ => return None,
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::chunks_exact_to_as_chunks, clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    const RGBA8: u32 = 8 | HAS_ALPHA | (RGB_MODE << 16);

    fn image(width: usize, height: usize, pixel: [f32; 4]) -> std::vec::Vec<f32> {
        let mut out = std::vec::Vec::with_capacity(width * height * 4);
        for _ in 0..width * height {
            out.extend_from_slice(&pixel);
        }
        out
    }

    fn alpha_count(pixels: &[f32]) -> usize {
        pixels.chunks_exact(4).filter(|pixel| pixel.get(3) == Some(&1.0)).count()
    }

    #[test]
    fn below_threshold_is_zero_rgba() {
        let mut pixels = image(1, 1, [0.8, 0.3, 0.1, 7.0 / 255.0]);
        let mut params = Params::default();
        params.cleanup = Cleanup::Off;
        process_pixels(&mut pixels, 1, 1, params);
        assert_eq!(pixels, [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn every_alpha_is_binary_and_every_hole_has_zero_rgb() {
        let mut pixels = std::vec::Vec::new();
        for y in 0..24 {
            for x in 0..32 {
                pixels.extend_from_slice(&[x as f32 / 31.0, y as f32 / 23.0, 0.4, ((x + y) % 11) as f32 / 10.0]);
            }
        }
        let mut params = Params::default();
        params.cleanup = Cleanup::Medium;
        process_pixels(&mut pixels, 32, 24, params);
        for pixel in pixels.chunks_exact(4) {
            let alpha = pixel.get(3).copied().unwrap_or(-1.0);
            assert!(alpha == 0.0 || alpha == 1.0, "fractional alpha: {pixel:?}");
            if alpha == 0.0 {
                assert_eq!(pixel, [0.0, 0.0, 0.0, 0.0]);
            }
        }
    }

    #[test]
    fn soft_mode_preserves_fully_opaque_pixels_exactly() {
        let original = [0.123, 0.456, 0.789, 1.0];
        let mut pixels = image(3, 3, original);
        let mut params = Params::default();
        params.cleanup = Cleanup::Strong;
        process_pixels(&mut pixels, 3, 3, params);
        for pixel in pixels.chunks_exact(4) {
            assert_eq!(pixel, original);
        }
    }

    #[test]
    fn partial_alpha_becomes_spatial_coverage() {
        let mut pixels = image(64, 64, [0.2, 0.6, 0.9, 0.42]);
        let mut params = Params::default();
        params.cleanup = Cleanup::Off;
        process_pixels(&mut pixels, 64, 64, params);
        let kept = alpha_count(&pixels);
        assert!(kept > 1_400 && kept < 2_050, "unexpected 42% coverage: {kept}");
        assert!(pixels.chunks_exact(4).all(|pixel| matches!(pixel.get(3), Some(0.0 | 1.0))));
    }

    #[test]
    fn full_artwork_mode_ventilates_opaque_pixels() {
        let mut pixels = image(64, 64, [0.25, 0.5, 0.75, 1.0]);
        let mut params = Params::default();
        params.processing_mode = ProcessingMode::FullArtwork;
        params.ventilation = 35.0;
        params.cleanup = Cleanup::Off;
        process_pixels(&mut pixels, 64, 64, params);
        let kept = alpha_count(&pixels);
        assert!(kept > 0 && kept < 64 * 64, "ventilation must introduce holes: {kept}");
    }

    #[test]
    fn round_and_line_screens_are_deterministic() {
        for pattern in [Pattern::RoundDots, Pattern::Lines] {
            let source = image(48, 37, [0.3, 0.7, 0.2, 0.57]);
            let (mut first, mut second) = (source.clone(), source);
            let mut params = Params::default();
            params.pattern = pattern;
            params.angle = 37.0;
            params.cleanup = Cleanup::Off;
            process_pixels(&mut first, 48, 37, params);
            process_pixels(&mut second, 48, 37, params);
            assert_eq!(first, second, "{pattern:?}");
        }
    }

    #[test]
    fn angle_changes_line_orientation() {
        let source = image(40, 24, [0.3, 0.5, 0.7, 0.5]);
        let (mut vertical, mut horizontal) = (source.clone(), source);
        let mut params = Params::default();
        params.pattern = Pattern::Lines;
        params.cleanup = Cleanup::Off;
        params.angle = 0.0;
        process_pixels(&mut vertical, 40, 24, params);
        params.angle = 90.0;
        process_pixels(&mut horizontal, 40, 24, params);
        assert_ne!(vertical, horizontal);
        let column = (0..24).map(|y| vertical[(y * 40) * 4 + 3]).collect::<std::vec::Vec<_>>();
        assert!(column.windows(2).all(|pair| pair[0] == pair[1]), "zero-degree lines run along y");
        let row = (0..40).map(|x| horizontal[x * 4 + 3]).collect::<std::vec::Vec<_>>();
        assert!(row.windows(2).all(|pair| pair[0] == pair[1]), "ninety-degree lines run along x");
    }

    #[test]
    fn knockout_removes_similar_color_but_keeps_different_color() {
        let mut pixels = [0.02, 0.02, 0.02, 1.0, 0.8, 0.1, 0.3, 1.0];
        let mut params = Params::default();
        params.knockout = Knockout::Black;
        params.knockout_tolerance = 32.0;
        params.cleanup = Cleanup::Off;
        process_pixels(&mut pixels, 2, 1, params);
        assert_eq!(&pixels[..4], &[0.0, 0.0, 0.0, 0.0]);
        assert_eq!(&pixels[4..], &[0.8, 0.1, 0.3, 1.0]);
    }

    #[test]
    fn custom_knockout_uses_user_rgb() {
        let mut pixels = image(1, 1, [64.0 / 255.0, 128.0 / 255.0, 192.0 / 255.0, 1.0]);
        let mut params = Params::default();
        params.knockout = Knockout::Custom;
        params.custom = [64.0 / 255.0, 128.0 / 255.0, 192.0 / 255.0];
        params.cleanup = Cleanup::Off;
        process_pixels(&mut pixels, 1, 1, params);
        assert_eq!(pixels, [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn cleanup_removes_an_isolated_speck_but_keeps_a_pair() {
        let mut isolated = image(5, 5, [0.0, 0.0, 0.0, 0.0]);
        isolated[12 * 4] = 0.4;
        isolated[12 * 4 + 3] = SCREENED;
        cleanup_in_place(&mut isolated, 5, 5, Cleanup::Light);
        normalize_alpha(&mut isolated);
        assert_eq!(isolated[12 * 4 + 3], 0.0);

        let mut pair = image(5, 5, [0.0, 0.0, 0.0, 0.0]);
        for at in [12usize, 13] {
            pair[at * 4] = 0.4;
            pair[at * 4 + 3] = SCREENED;
        }
        cleanup_in_place(&mut pair, 5, 5, Cleanup::Light);
        normalize_alpha(&mut pair);
        assert_eq!(pair[12 * 4 + 3], 1.0);
        assert_eq!(pair[13 * 4 + 3], 1.0);
    }

    #[test]
    fn stronger_cleanup_levels_apply_the_documented_local_minimum() {
        let mut pair = image(7, 7, [0.0, 0.0, 0.0, 0.0]);
        for at in [24usize, 25] {
            pair[at * 4] = 0.4;
            pair[at * 4 + 3] = SCREENED;
        }
        cleanup_in_place(&mut pair, 7, 7, Cleanup::Medium);
        normalize_alpha(&mut pair);
        assert_eq!(alpha_count(&pair), 0, "Medium removes a two-pixel island");

        let mut four = image(7, 7, [0.0, 0.0, 0.0, 0.0]);
        for at in [16usize, 17, 23, 24] {
            four[at * 4] = 0.4;
            four[at * 4 + 3] = SCREENED;
        }
        cleanup_in_place(&mut four, 7, 7, Cleanup::Strong);
        normalize_alpha(&mut four);
        assert_eq!(alpha_count(&four), 0, "Strong removes a four-pixel island");

        let mut five = image(7, 7, [0.0, 0.0, 0.0, 0.0]);
        for at in [16usize, 17, 23, 24, 25] {
            five[at * 4] = 0.4;
            five[at * 4 + 3] = SCREENED;
        }
        cleanup_in_place(&mut five, 7, 7, Cleanup::Strong);
        normalize_alpha(&mut five);
        assert_eq!(alpha_count(&five), 5, "Strong retains a compact five-pixel island");
    }

    #[test]
    fn threshold_boundary_and_non_finite_alpha_are_safe() {
        let mut pixels = [0.2, 0.4, 0.6, 1.0, 0.9, 0.8, 0.7, 0.999, 0.4, 0.3, 0.2, f32::NAN, 0.1, 0.2, 0.3, -1.0];
        let mut params = Params::default();
        params.alpha_threshold = 255.0;
        params.cleanup = Cleanup::Off;
        process_pixels(&mut pixels, 4, 1, params);
        assert_eq!(&pixels[..4], &[0.2, 0.4, 0.6, 1.0], "alpha exactly at threshold survives");
        for pixel in pixels[4..].chunks_exact(4) {
            assert_eq!(pixel, [0.0, 0.0, 0.0, 0.0]);
        }
    }

    #[test]
    fn shadow_boost_is_optional_bounded_and_deterministic() {
        let source = [0.30, 0.08, 0.18, 1.0];
        let mut unchanged = source;
        let mut params = Params::default();
        params.cleanup = Cleanup::Off;
        process_pixels(&mut unchanged, 1, 1, params);
        assert_eq!(unchanged, source);

        params.shadow_boost = 60.0;
        let (mut first, mut second) = (source, source);
        process_pixels(&mut first, 1, 1, params);
        process_pixels(&mut second, 1, 1, params);
        assert_eq!(first, second);
        assert_ne!(first, source);
        assert!(first[..3].iter().all(|value| value.is_finite() && (0.0..=1.0).contains(value)));
        assert_eq!(first[3], 1.0);
    }

    fn padded(source: &[f32], width: usize, height: usize, x0: i32, y0: i32, band_width: usize, band_height: usize) -> std::vec::Vec<f32> {
        let mut out = std::vec::Vec::with_capacity(band_width * band_height * 4);
        for y in 0..band_height {
            for x in 0..band_width {
                let sx = x0.saturating_add(x as i32).clamp(0, width as i32 - 1) as usize;
                let sy = y0.saturating_add(y as i32).clamp(0, height as i32 - 1) as usize;
                let at = (sy * width + sx) * 4;
                out.extend_from_slice(source.get(at..at + 4).unwrap_or(&[0.0; 4]));
            }
        }
        out
    }

    fn process_band(source: &[f32], width: usize, height: usize, x: usize, band_width: usize, params: Params) -> std::vec::Vec<f32> {
        let overlap = 2usize;
        let input_x = x as i32 - overlap as i32;
        let input_y = -(overlap as i32);
        let input_width = band_width + overlap * 2;
        let input_height = height + overlap * 2;
        let mut data = padded(source, width, height, input_x, input_y, input_width, input_height);
        let mut local = params;
        local.origin_x = input_x;
        local.origin_y = input_y;
        process_pixels(&mut data, input_width, input_height, local);
        let mut inner = std::vec::Vec::with_capacity(band_width * height * 4);
        for row in 0..height {
            let at = ((row + overlap) * input_width + overlap) * 4;
            inner.extend_from_slice(data.get(at..at + band_width * 4).unwrap_or(&[]));
        }
        inner
    }

    #[test]
    fn separate_bands_match_one_band_with_global_coordinates_and_cleanup() {
        let (width, height, split) = (37usize, 29usize, 16usize);
        let mut source = std::vec::Vec::with_capacity(width * height * 4);
        for y in 0..height {
            for x in 0..width {
                source.extend_from_slice(&[x as f32 / width as f32, y as f32 / height as f32, 0.6, 0.54]);
            }
        }
        let mut params = Params::default();
        params.angle = 31.0;
        params.cleanup = Cleanup::Strong;
        let whole = process_band(&source, width, height, 0, width, params);
        let left = process_band(&source, width, height, 0, split, params);
        let right = process_band(&source, width, height, split, width - split, params);
        let mut joined = std::vec::Vec::with_capacity(whole.len());
        for row in 0..height {
            let la = row * split * 4;
            let ra = row * (width - split) * 4;
            joined.extend_from_slice(&left[la..la + split * 4]);
            joined.extend_from_slice(&right[ra..ra + (width - split) * 4]);
        }
        assert_eq!(joined, whole);
    }

    #[test]
    fn malformed_or_unsupported_calls_fail_safely() {
        let mut pixels = image(2, 2, [0.2, 0.4, 0.6, 0.8]);
        assert_eq!(filter_impl(&mut pixels[..15], 2, 2, 4, RGBA8, b"{}"), ERR_BUFFER);
        assert_eq!(filter_impl(&mut pixels, 2, 2, 4, 8 | (RGB_MODE << 16), b"{}"), ERR_UNSUPPORTED_FORMAT);
        assert_eq!(filter_impl(&mut pixels, 2, 2, 4, RGBA8, b"not json"), ERR_PARAMS);
    }

    #[test]
    fn host_parameter_json_parses_without_allocation() {
        let json = br#"{
          "alphaThreshold":8,"processingMode":"fullArtworkForcedHalftone","pattern":"lines",
          "cellSize":12,"angle":45.5,"ventilationPercent":30,"knockout":"custom",
          "customR":10,"customG":20,"customB":30,"knockoutTolerance":18,
          "shadowColorBoost":12,"speckCleanup":"strong",
          "_image":{"x":-2,"y":254,"width":260,"height":260,"overlap":2,
                    "canvasWidth":1000,"canvasHeight":800,"mode":"rgb","depth":8,"alpha":true,"channels":4}}
        "#;
        let params = parse_params(json, 260, 260, 4, RGBA8).expect("valid host JSON");
        assert_eq!(params.processing_mode, ProcessingMode::FullArtwork);
        assert_eq!(params.pattern, Pattern::Lines);
        assert_eq!(params.knockout, Knockout::Custom);
        assert_eq!(params.cleanup, Cleanup::Strong);
        assert_eq!((params.origin_x, params.origin_y), (-2, 254));
        assert_eq!(params.cell_size, 12.0);
    }

    #[test]
    fn manifest_exposes_operator_controls_and_fixed_overlap() {
        for text in [
            "cz.optimaprint.dtfprep",
            "alphaThreshold",
            "softTransparencyOnly",
            "fullArtworkForcedHalftone",
            "roundDots",
            "lines",
            "cellSize",
            "knockoutTolerance",
            "shadowColorBoost",
            "speckCleanup",
            "\"overlap\": 2",
        ] {
            assert!(MANIFEST.contains(text), "missing {text}");
        }
    }

    #[test]
    fn malformed_parameter_objects_are_rejected_before_pixels_change() {
        let valid = r#"{
          "alphaThreshold":8,"processingMode":"softTransparencyOnly","pattern":"roundDots",
          "cellSize":8,"angle":22.5,"ventilationPercent":20,"knockout":"off",
          "customR":0,"customG":0,"customB":0,"knockoutTolerance":24,
          "shadowColorBoost":0,"speckCleanup":"light",
          "_image":{"x":-2,"y":-2,"width":6,"height":6,"overlap":2,
                    "canvasWidth":2,"canvasHeight":2,"mode":"rgb","depth":8,"alpha":true,"channels":4}}
        "#;
        let malformed = [
            "{}".to_string(),
            valid.trim_end()[..valid.trim_end().len() - 1].to_string(),
            format!("{valid} trailing"),
            valid.replacen("\"alphaThreshold\":8", "\"alphaThreshold\":8.", 1),
            valid.replacen("\"alphaThreshold\":8", "\"alphaThreshold\":8junk", 1),
            valid.replacen("\"alphaThreshold\":8", "\"alphaThreshold\":8,\"alphaThreshold\":9", 1),
            valid.replacen("\"_image\"", "\"ignoredImage\"", 1),
            valid.replacen("\"alphaThreshold\":8", "\"unknown\":[[[[[[[[[0]]]]]]]]],\"alphaThreshold\":8", 1),
        ];
        for json in malformed {
            let mut pixels = image(6, 6, [0.2, 0.4, 0.6, 0.8]);
            let before = pixels.clone();
            assert_eq!(filter_impl(&mut pixels, 6, 6, 4, RGBA8, json.as_bytes()), ERR_PARAMS, "{json}");
            assert_eq!(pixels, before, "an invalid call must be transactional");
        }
    }
}
