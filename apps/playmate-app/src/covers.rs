//! Cover art for the game library.
//!
//! Per title, resolution order is: an image already in the covers directory
//! (user-provided or previously downloaded), then a download from the
//! libretro-thumbnails NES boxart collection. Remote names follow the
//! No-Intro convention ("Contra (USA).png"), so matching first fetches the
//! collection's file index once and compares normalized base names.
//!
//! All I/O runs on one background worker thread; decoded RGBA images arrive
//! on a channel and the UI thread uploads them as textures in [`CoverStore::poll`].
//! A confirmed miss is remembered for the session so it is not retried.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

/// libretro-thumbnails NES boxart directory (No-Intro named PNG files).
const BOXART_BASE: &str = "https://thumbnails.libretro.com/Nintendo%20-%20Nintendo%20Entertainment%20System/Named_Boxarts/";

/// Cached copy of the remote file index, one file name per line.
const INDEX_FILE: &str = ".libretro-index";

/// Decoded covers wider than this are downsampled before texture upload.
const MAX_TEXTURE_WIDTH: usize = 256;

/// Largest accepted download body; guards against a misbehaving server.
const MAX_DOWNLOAD_BYTES: u64 = 8 * 1024 * 1024;

/// Decoded RGBA8 cover image produced by the worker.
struct CoverImage {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

/// UI-side cover cache backed by a background lookup worker.
pub struct CoverStore {
    /// Uploaded textures; `None` records a confirmed miss for the session.
    textures: HashMap<String, Option<egui::TextureHandle>>,
    /// Titles already handed to the worker, to queue each title only once.
    requested: HashSet<String>,
    /// Lookup requests to the worker.
    tx: mpsc::Sender<String>,
    /// Decoded results from the worker.
    rx: mpsc::Receiver<(String, Option<CoverImage>)>,
}

impl CoverStore {
    /// Creates the store and starts the background lookup worker.
    pub fn new() -> Self {
        let (tx, work_rx) = mpsc::channel::<String>();
        let (work_tx, rx) = mpsc::channel();
        std::thread::spawn(move || worker(work_rx, work_tx));
        Self {
            textures: HashMap::new(),
            requested: HashSet::new(),
            tx,
            rx,
        }
    }

    /// Queues cover lookups for titles not seen before.
    pub fn ensure<'a>(&mut self, titles: impl Iterator<Item = &'a str>) {
        for title in titles {
            if self.requested.insert(title.to_string()) {
                let _ = self.tx.send(title.to_string());
            }
        }
    }

    /// Uploads finished covers as textures; call once per frame while the
    /// library page is visible. Requests a repaint when anything arrived.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let mut arrived = false;
        while let Ok((title, image)) = self.rx.try_recv() {
            let texture = image.map(|img| {
                let color =
                    egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &img.rgba);
                ctx.load_texture(format!("cover:{title}"), color, Default::default())
            });
            self.textures.insert(title, texture);
            arrived = true;
        }
        if arrived {
            ctx.request_repaint();
        }
    }

    /// Uploaded texture for a title; `None` while pending or after a miss.
    pub fn get(&self, title: &str) -> Option<&egui::TextureHandle> {
        self.textures.get(title).and_then(|t| t.as_ref())
    }

    /// Forgets recorded misses so the next [`ensure`](Self::ensure) retries
    /// them; wired to the library's refresh action for when covers were
    /// added on disk or the network came back.
    pub fn retry_misses(&mut self) {
        let misses: Vec<String> = self
            .textures
            .iter()
            .filter(|(_, texture)| texture.is_none())
            .map(|(title, _)| title.clone())
            .collect();
        for title in misses {
            self.textures.remove(&title);
            self.requested.remove(&title);
        }
    }
}

/// Background loop: resolves each requested title and sends the result back.
/// Exits when the UI side drops its sender.
fn worker(rx: mpsc::Receiver<String>, tx: mpsc::Sender<(String, Option<CoverImage>)>) {
    let dir = crate::config::covers_dir();
    // The remote index is fetched lazily on the first title that needs it.
    let mut index: Option<Vec<String>> = None;
    while let Ok(title) = rx.recv() {
        let image = local_cover(&dir, &title).or_else(|| download_cover(&dir, &title, &mut index));
        if tx.send((title, image)).is_err() {
            return;
        }
    }
}

/// Decodes `covers/<title>.png` when present.
fn local_cover(dir: &Path, title: &str) -> Option<CoverImage> {
    let path = dir.join(format!("{title}.png"));
    let bytes = std::fs::read(&path).ok()?;
    match decode_png(&bytes) {
        Ok(image) => Some(image),
        Err(e) => {
            log::warn!("ignoring unreadable cover {path:?}: {e}");
            None
        }
    }
}

/// Matches the title against the remote index, downloads the boxart, caches
/// it beside the local covers, and decodes it. Any failure is a miss.
fn download_cover(dir: &Path, title: &str, index: &mut Option<Vec<String>>) -> Option<CoverImage> {
    let names = match index {
        Some(names) => names,
        None => index.insert(load_index(dir)),
    };
    let remote = match_name(title, names)?;
    let url = format!("{BOXART_BASE}{}", encode_component(remote));
    let bytes = match http_get(&url) {
        Ok(bytes) => bytes,
        Err(e) => {
            log::warn!("cover download failed for {title}: {e}");
            return None;
        }
    };
    let image = match decode_png(&bytes) {
        Ok(image) => image,
        Err(e) => {
            log::warn!("cover decode failed for {title}: {e}");
            return None;
        }
    };
    // Cache the original bytes so the next launch resolves locally.
    if std::fs::create_dir_all(dir).is_ok()
        && let Err(e) = std::fs::write(dir.join(format!("{title}.png")), &bytes)
    {
        log::warn!("cover cache write failed for {title}: {e}");
    }
    log::info!("cover downloaded: {title} <- {remote}");
    Some(image)
}

/// Returns the remote boxart index, reading the cached copy when present and
/// otherwise fetching and caching the directory listing. A fetch failure
/// yields an empty index, so every lookup of this session becomes a miss.
fn load_index(dir: &Path) -> Vec<String> {
    let cache = dir.join(INDEX_FILE);
    if let Ok(text) = std::fs::read_to_string(&cache) {
        return text.lines().map(str::to_string).collect();
    }
    let html = match http_get(BOXART_BASE) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => {
            log::warn!("cover index fetch failed: {e}");
            return Vec::new();
        }
    };
    let names = parse_listing(&html);
    log::info!("cover index fetched: {} entries", names.len());
    if std::fs::create_dir_all(dir).is_ok()
        && let Err(e) = std::fs::write(&cache, names.join("\n"))
    {
        log::warn!("cover index cache write failed: {e}");
    }
    names
}

/// Fetches a URL fully into memory with a global timeout.
fn http_get(url: &str) -> anyhow::Result<Vec<u8>> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut body = Vec::new();
    agent
        .get(url)
        .call()?
        .body_mut()
        .as_reader()
        .take(MAX_DOWNLOAD_BYTES)
        .read_to_end(&mut body)?;
    Ok(body)
}

/// Extracts percent-decoded `*.png` file names from an autoindex HTML page.
fn parse_listing(html: &str) -> Vec<String> {
    let mut names = Vec::new();
    for chunk in html.split("href=\"").skip(1) {
        let Some(href) = chunk.split('"').next() else {
            continue;
        };
        if !href.ends_with(".png") {
            continue;
        }
        let name = percent_decode(href);
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Percent-encodes one path segment (RFC 3986 unreserved characters pass through).
fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Decodes percent-encoded bytes; malformed escapes pass through verbatim.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Strips a trailing `.png` and every trailing `(...)`/`[...]` tag group,
/// leaving the base game name: `"Contra (USA) [!].png"` -> `"Contra"`.
fn base_name(name: &str) -> &str {
    let mut base = name.strip_suffix(".png").unwrap_or(name).trim_end();
    loop {
        let stripped = base.trim_end();
        let Some(open) = stripped.rfind(['(', '[']) else {
            break;
        };
        let tail = &stripped[open..];
        let closed = (tail.starts_with('(') && tail.ends_with(')'))
            || (tail.starts_with('[') && tail.ends_with(']'));
        if !closed || open == 0 {
            break;
        }
        base = stripped[..open].trim_end();
    }
    base
}

/// Case- and punctuation-insensitive comparison key for game names.
fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Region preference when several releases share a base name; lower is better.
fn region_rank(name: &str) -> usize {
    const PREFERRED: [&str; 4] = ["(USA", "(World", "(Europe", "(Japan"];
    PREFERRED
        .iter()
        .position(|tag| name.contains(tag))
        .unwrap_or(PREFERRED.len())
}

/// Common Chinese titles of popular NES games mapped to their English
/// collection names, because the libretro index is English-only while many
/// ROM libraries use Chinese file names. Applied to the tag-stripped base
/// name before matching; every entry is verified against the live index.
const ZH_ALIASES: &[(&str, &str)] = &[
    ("魂斗罗", "Contra"),
    ("魂斗罗1", "Contra"),
    ("魂斗罗2", "Super Contra"),
    ("超级魂斗罗", "Super Contra"),
    ("魂斗罗力量", "Contra Force"),
    ("超级玛丽", "Super Mario Bros."),
    ("超级玛丽1", "Super Mario Bros."),
    ("超级马里奥", "Super Mario Bros."),
    ("超级玛丽2", "Super Mario Bros. 2"),
    ("超级玛丽3", "Super Mario Bros. 3"),
    ("马里奥兄弟", "Mario Bros."),
    ("双截龙", "Double Dragon"),
    ("双截龙1", "Double Dragon"),
    ("双截龙2", "Double Dragon II"),
    ("双截龙3", "Double Dragon III"),
    ("忍者神龟", "Teenage Mutant Ninja Turtles"),
    ("忍者神龟1", "Teenage Mutant Ninja Turtles"),
    ("忍者神龟2", "Teenage Mutant Ninja Turtles II"),
    ("忍者神龟3", "Teenage Mutant Ninja Turtles III"),
    ("忍者龙剑传", "Ninja Gaiden"),
    ("忍者龙剑传1", "Ninja Gaiden"),
    ("忍者龙剑传2", "Ninja Gaiden II"),
    ("忍者龙剑传3", "Ninja Gaiden III"),
    ("雪人兄弟", "Snow Brothers"),
    ("坦克大战", "Battle City"),
    ("冒险岛", "Adventure Island"),
    ("冒险岛1", "Adventure Island"),
    ("高桥名人冒险岛", "Adventure Island"),
    ("冒险岛2", "Adventure Island II"),
    ("冒险岛3", "Adventure Island 3"),
    ("松鼠大战", "Chip 'n Dale Rescue Rangers"),
    ("松鼠大战2", "Chip 'n Dale Rescue Rangers 2"),
    ("赤色要塞", "Jackal"),
    ("绿色兵团", "Rush'n Attack"),
    ("沙罗曼蛇", "Salamander"),
    ("宇宙巡航机", "Gradius"),
    ("兵蜂", "TwinBee"),
    ("炸弹人", "Bomberman"),
    ("泡泡龙", "Bubble Bobble"),
    ("淘金者", "Lode Runner"),
    ("马戏团", "Circus Charlie"),
    ("敲冰块", "Ice Climber"),
    ("气球大战", "Balloon Fight"),
    ("打鸭子", "Duck Hunt"),
    ("大金刚", "Donkey Kong"),
    ("洛克人", "Mega Man"),
    ("洛克人2", "Mega Man 2"),
    ("洛克人3", "Mega Man 3"),
    ("洛克人4", "Mega Man 4"),
    ("洛克人5", "Mega Man 5"),
    ("洛克人6", "Mega Man 6"),
    ("恶魔城", "Castlevania"),
    ("恶魔城2", "Castlevania II"),
    ("恶魔城3", "Castlevania III"),
    ("月风魔传", "Getsu Fuuma Den"),
    ("热血物语", "River City Ransom"),
    ("热血足球", "Nintendo World Cup"),
    ("热血格斗", "Nekketsu Kakutou Densetsu"),
    ("热血躲避球", "Super Dodge Ball"),
    ("热血硬派", "Renegade"),
    ("快打旋风", "Mighty Final Fight"),
    ("七宝奇谋", "Goonies"),
    ("古巴战士", "Guerrilla War"),
    ("加纳战机", "Gun Nac"),
    ("唐老鸭梦冒险", "DuckTales"),
    ("唐老鸭", "DuckTales"),
    ("彩虹岛", "Rainbow Islands"),
    ("飞龙之拳", "Hiryuu no Ken"),
    ("越野机车", "Excitebike"),
    ("火箭车", "Road Fighter"),
    ("勇者斗恶龙", "Dragon Warrior"),
    ("最终幻想", "Final Fantasy"),
    ("塞尔达传说", "Legend of Zelda"),
    ("银河战士", "Metroid"),
    ("光之神话", "Kid Icarus"),
    ("星之卡比", "Kirby's Adventure"),
    ("魔界村", "Ghosts'n Goblins"),
    ("功夫", "Kung Fu"),
    ("荒野大镖客", "Wild Gunman"),
    ("小蜜蜂", "Galaxian"),
    ("铁板阵", "Xevious"),
    ("影子传说", "Legend of Kage"),
    ("圣斗士", "Saint Seiya"),
    ("北斗神拳", "Hokuto no Ken"),
    ("街头霸王2010", "Street Fighter 2010"),
    ("吞食天地", "Tenchi wo Kurau"),
    ("吞食天地2", "Tenchi wo Kurau II"),
    ("重装机兵", "Metal Max"),
    ("南极大冒险", "Kekkyoku Nankyoku Daibouken"),
    ("大力水手", "Popeye"),
    ("俄罗斯方块", "Tetris"),
    ("脱狱", "P.O.W. - Prisoners of War"),
];

/// Translates a Chinese base name to its English collection name, if known.
fn zh_alias(base: &str) -> Option<&'static str> {
    let key = base.trim();
    ZH_ALIASES
        .iter()
        .find(|(zh, _)| *zh == key)
        .map(|(_, en)| *en)
}

/// Minimum normalized-key length for the prefix fallback, so short names
/// like "Kage" cannot pair with unrelated longer titles.
const PREFIX_MATCH_MIN: usize = 6;

/// Finds the remote file matching a ROM title by normalized base name,
/// preferring USA/World releases among candidates. When nothing matches
/// exactly, falls back to a prefix match because collection names often
/// append a subtitle ("Double Dragon II - The Revenge") that ROM file
/// names omit; the best region and then the shortest name win.
fn match_name<'a>(title: &str, index: &'a [String]) -> Option<&'a str> {
    let base = base_name(title);
    let base = zh_alias(base).unwrap_or(base);
    let key = normalize(base);
    if key.is_empty() {
        return None;
    }
    let exact = index
        .iter()
        .filter(|name| normalize(base_name(name)) == key)
        .min_by_key(|name| region_rank(name));
    if let Some(name) = exact {
        return Some(name);
    }
    if key.len() < PREFIX_MATCH_MIN {
        return None;
    }
    index
        .iter()
        .filter(|name| normalize(base_name(name)).starts_with(&key))
        .min_by_key(|name| (region_rank(name), normalize(base_name(name)).len()))
        .map(String::as_str)
}

/// Decodes a PNG into RGBA8, expanding palette and grayscale forms and
/// downsampling anything wider than [`MAX_TEXTURE_WIDTH`].
fn decode_png(bytes: &[u8]) -> anyhow::Result<CoverImage> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info()?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| anyhow::anyhow!("图片尺寸异常"))?;
    let mut buf = vec![0u8; size];
    let info = reader.next_frame(&mut buf)?;
    buf.truncate(info.buffer_size());

    let width = info.width as usize;
    let height = info.height as usize;
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 0xFF])
            .collect(),
        png::ColorType::GrayscaleAlpha => buf
            .chunks_exact(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 0xFF]).collect(),
        other => anyhow::bail!("unsupported color type after expansion: {other:?}"),
    };
    Ok(shrink(CoverImage {
        width,
        height,
        rgba,
    }))
}

/// Nearest-neighbor downsample to [`MAX_TEXTURE_WIDTH`] for oversized covers.
fn shrink(image: CoverImage) -> CoverImage {
    if image.width <= MAX_TEXTURE_WIDTH || image.height == 0 {
        return image;
    }
    let width = MAX_TEXTURE_WIDTH;
    let height = (image.height * width / image.width).max(1);
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        let src_y = y * image.height / height;
        for x in 0..width {
            let src_x = x * image.width / width;
            let i = (src_y * image.width + src_x) * 4;
            rgba.extend_from_slice(&image.rgba[i..i + 4]);
        }
    }
    CoverImage {
        width,
        height,
        rgba,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// Tag groups and the extension are stripped down to the base name.
    #[test]
    fn base_name_strips_tags() {
        assert_eq!(base_name("Contra (USA).png"), "Contra");
        assert_eq!(base_name("Super Mario Bros. (World)"), "Super Mario Bros.");
        assert_eq!(base_name("Contra (U) [!]"), "Contra");
        assert_eq!(base_name("Contra"), "Contra");
        // An unclosed group and a leading group are left alone.
        assert_eq!(base_name("(strange"), "(strange");
        assert_eq!(base_name("(Whole Name)"), "(Whole Name)");
    }

    /// Normalization ignores case, punctuation, and spacing.
    #[test]
    fn normalize_is_punctuation_insensitive() {
        assert_eq!(normalize("Super Mario Bros."), "supermariobros");
        assert_eq!(normalize("ROCKMAN 2"), "rockman2");
        assert_eq!(normalize("魂斗罗"), "");
    }

    /// Matching pairs a plain ROM title with its No-Intro release,
    /// preferring the USA version among candidates.
    #[test]
    fn match_name_prefers_usa_release() {
        let index = vec![
            "Contra (Japan).png".to_string(),
            "Contra (USA).png".to_string(),
            "Contra Force (USA).png".to_string(),
        ];
        assert_eq!(match_name("Contra", &index), Some("Contra (USA).png"));
        assert_eq!(
            match_name("contra (U) [!]", &index),
            Some("Contra (USA).png")
        );
        assert_eq!(match_name("Probotector", &index), None);
    }

    /// Known Chinese titles translate through the alias table before
    /// matching; unknown ones normalize to an empty key and miss.
    #[test]
    fn match_name_translates_chinese_aliases() {
        let index = vec![
            "Contra (USA).png".to_string(),
            "Snow Brothers (USA).png".to_string(),
        ];
        assert_eq!(match_name("魂斗罗", &index), Some("Contra (USA).png"));
        // Trailing tags are stripped before the alias lookup.
        assert_eq!(
            match_name("雪人兄弟 (中文版)", &index),
            Some("Snow Brothers (USA).png")
        );
        assert_eq!(match_name("未知中文游戏", &index), None);
    }

    /// A plain main title prefix-matches its subtitled collection name;
    /// an official region release outranks a shorter unofficial entry, and
    /// short keys never use the prefix fallback.
    #[test]
    fn match_name_falls_back_to_subtitled_prefix() {
        let index = vec![
            "Zelda II - Amida's Curse.png".to_string(),
            "Zelda II - The Adventure of Link (USA).png".to_string(),
            "Double Dragon II - The Revenge (USA).png".to_string(),
            "Kagerou Densetsu (Japan).png".to_string(),
        ];
        assert_eq!(
            match_name("Zelda II", &index),
            Some("Zelda II - The Adventure of Link (USA).png")
        );
        assert_eq!(
            match_name("Double Dragon II", &index),
            Some("Double Dragon II - The Revenge (USA).png")
        );
        // "Kage" (4 letters) is below the prefix threshold.
        assert_eq!(match_name("Kage", &index), None);
    }

    /// Autoindex listings yield percent-decoded PNG names only.
    #[test]
    fn parse_listing_extracts_png_names() {
        let html = r#"<a href="../">../</a>
<a href="Contra%20(USA).png">Contra (USA).png</a>
<a href="Notes.txt">Notes.txt</a>"#;
        assert_eq!(parse_listing(html), vec!["Contra (USA).png".to_string()]);
    }

    /// Encoding round-trips a name with spaces and parentheses.
    #[test]
    fn encode_decode_roundtrip() {
        let name = "Contra (USA).png";
        assert_eq!(percent_decode(&encode_component(name)), name);
        assert_eq!(encode_component(" "), "%20");
    }

    /// An encoded RGB PNG decodes to opaque RGBA with the original pixels.
    #[test]
    fn decode_png_expands_rgb_to_rgba() {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 2, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[10, 20, 30, 40, 50, 60]).unwrap();
        }
        let image = decode_png(&bytes).unwrap();
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.rgba, vec![10, 20, 30, 0xFF, 40, 50, 60, 0xFF]);
    }

    /// RGB pixels are expanded to opaque RGBA and oversized covers shrink.
    #[test]
    fn shrink_halves_oversized_images() {
        let image = CoverImage {
            width: MAX_TEXTURE_WIDTH * 2,
            height: 4,
            rgba: vec![0x80; MAX_TEXTURE_WIDTH * 2 * 4 * 4],
        };
        let out = shrink(image);
        assert_eq!(out.width, MAX_TEXTURE_WIDTH);
        assert_eq!(out.height, 2);
        assert_eq!(out.rgba.len(), MAX_TEXTURE_WIDTH * 2 * 4);
    }
}
