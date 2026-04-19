/******************************************************************************

COPYRIGHT 2026, Seho Lee.

THIS FILE IS PROVIDED BY THE COPYRIGHT HOLDERS UNDER THE FOLLOWING TERMS:

1. ANY PERSON, GROUP, COMPANY, GOVERNMENT, OR OTHER LEGAL ENTITY THAT OBTAINS A
   COPY OF THIS FILE IS PERMITTED TO USE, REPRODUCE, MODIFY, AND DISTRIBUTE IT
   FOR ANY LAWFUL PURPOSE, INCLUDING COMMERCIAL USE.

2. ANY REPRODUCTION, MODIFICATION, AND DISTRIBUTION OF THIS FILE MUST RETAIN THE
   COPYRIGHT NOTICE ABOVE. IF THIS FILE IS USED IN A PRODUCT, THE COPYRIGHT
   NOTICE MUST BE INCLUDED IN THE DOCUMENTATION OR OTHER MATERIALS PROVIDED
   WITH THE PRODUCT, WHERE REASONABLY PRACTICABLE.

3. THIS FILE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
   IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
   FITNESS FOR A PARTICULAR PURPOSE, AND NONINFRINGEMENT. TO THE MAXIMUM EXTENT
   PERMITTED BY APPLICABLE LAW, THE AUTHORS OR COPYRIGHT HOLDERS SHALL NOT BE
   LIABLE FOR ANY CLAIM, DAMAGES, OR OTHER LIABILITY ARISING FROM, OUT OF, OR
   IN CONNECTION WITH THE USE OF THIS FILE.

END OF TERMS

******************************************************************************/

use image::{
    ExtendedColorType, ImageEncoder,
    codecs::{jpeg::JpegEncoder, png::PngEncoder},
};
use lcms2::{Intent, PixelFormat, Profile, Transform};
use num_cpus::get;
use rayon::{ThreadPoolBuilder, prelude::*};
#[cfg(feature = "exif")]
use rexiv2::Metadata;
use rsraw::RawImage;
use serde::Deserialize;
use std::{
    env,
    fs::{self, File, create_dir_all},
    io::{self, BufWriter, Read, Write},
    ops::Deref,
    path::{Path, PathBuf},
    u8,
};
use walkdir::WalkDir;

// ================= DEFAULT CONFIG =================

const DEFAULT_CONFIG: &str = include_str!("config.toml");

// ================= CONFIG =================

#[derive(Deserialize)]
struct Config {
    storage_root: String,
    outputs: Vec<OutputConfig>,
    #[cfg(feature = "exif")]
    metadata: Option<MetadataConfig>,
}

#[derive(Deserialize)]
struct OutputConfig {
    suffix: String,
    format: String,
    quality: Option<u8>,
    icc: Option<String>,
}

#[cfg(feature = "exif")]
#[derive(Deserialize)]
struct MetadataConfig {
    copy_exif: Option<bool>,
}

// ================= MAIN =================

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = load_or_create_config()?;

    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        println!("COPYRIGHT 2026, Seho Lee.");
        println!("Usage: {} <name>", args[0]);
        return Ok(());
    }

    let name = &args[1];

    let base = PathBuf::from(&config.storage_root).join(name);
    create_dir_all(&base)?;

    let files: Vec<PathBuf> = WalkDir::new(".")
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map_or(false, |e| e.eq_ignore_ascii_case("SRW"))
        })
        .map(|e| e.into_path())
        .collect();

    ThreadPoolBuilder::new()
        .num_threads(get())
        .build_global()
        .ok();

    files.par_iter().for_each(|f| {
        if let Err(err) = process_file(f, &base, &config) {
            println!("Error occurred when processing {}: {}", f.display(), err)
        }
    });

    Ok(())
}

// ================= CONFIG LOADER =================

fn load_or_create_config() -> Result<Config, Box<dyn std::error::Error>> {
    let home = dirs::home_dir().ok_or("No home directory")?;
    let cfg_dir = home.join(".nphotoman");
    let cfg_file = cfg_dir.join("config.toml");

    if !cfg_dir.exists() {
        create_dir_all(&cfg_dir)?;
    }

    if !cfg_file.exists() {
        let mut f = File::create(&cfg_file)?;
        f.write_all(DEFAULT_CONFIG.as_bytes())?;
        println!("Created config: {}", cfg_file.display());
    }

    let mut s = String::new();
    File::open(&cfg_file)?.read_to_string(&mut s)?;

    let config: Config = toml::from_str(&s)?;
    Ok(config)
}

// ================= CORE =================

fn process_file(
    raw_path: &Path,
    base: &Path,
    config: &Config,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("Processing {}", raw_path.display());

    // RAW decode
    let result: Result<Vec<u8>, io::Error> = fs::read(raw_path);
    if let Err(err) = result {
        return Result::Err(err.into());
    }
    let result = &result.unwrap();
    let result = RawImage::open(result);
    if let Err(err) = result {
        return Result::Err(err.into());
    }

    let mut raw = result.unwrap();
    raw.set_use_camera_wb(true);
    raw.set_use_camera_matrix(true);
    raw.as_mut().params.output_color = 4;
    raw.unpack()?;

    let width = raw.width() as usize;
    let height = raw.height() as usize;
    let result = raw.process::<16>();
    if let Err(err) = result {
        return Result::Err(err.into());
    }

    let img = result.unwrap();
    let buf: Vec<u8> = img.deref().iter().flat_map(|e| e.to_ne_bytes()).collect();
    let stem = String::from(raw_path.file_stem().unwrap().to_str().unwrap());

    for out in &config.outputs {
        // ICC load
        let icc_data = if let Some(path) = &out.icc {
            Some(fs::read(path)?)
        } else {
            None
        };

        let mut nbuf = vec![0u8; buf.len()];
        if let Some(ref icc) = icc_data {
            let transform = Transform::new(
                &Profile::new_icc(include_bytes!("ProPhoto-RGB.icc"))?,
                PixelFormat::RGB_16,
                &Profile::new_icc(icc)?,
                PixelFormat::RGB_16,
                Intent::Perceptual,
            )?;
            transform.transform_pixels(&buf, &mut nbuf);
        } else {
            nbuf.copy_from_slice(&buf);
        }

        match out.format.as_str() {
            "jpeg" => {
                let path = base.join(format!("{}-{}.jpeg", stem, out.suffix));
                let mut writer = BufWriter::new(File::create(&path)?);
                let mut enc = JpegEncoder::new_with_quality(&mut writer, out.quality.unwrap_or(90));

                if let Some(ref icc) = icc_data {
                    let _ = enc.set_icc_profile(icc.clone());
                }

                let mut nbuf8: Vec<u8> = nbuf
                    .chunks_exact(2)
                    .map(|e| (u16::from_ne_bytes([e[0], e[1]]) >> 8).try_into().unwrap())
                    .collect();

                enc.write_image(
                    nbuf8.by_ref(),
                    width.try_into().unwrap(),
                    height.try_into().unwrap(),
                    ExtendedColorType::Rgb8.into(),
                )?;
                copy_metadata(raw_path, &path, config)?;
            }

            "png" => {
                let path = base.join(format!("{}-{}.png", stem, out.suffix));
                let mut enc = PngEncoder::new(File::create(&path)?);

                if let Some(ref icc) = icc_data {
                    let _ = enc.set_icc_profile(icc.clone());
                }

                enc.write_image(
                    nbuf.by_ref(),
                    width.try_into().unwrap(),
                    height.try_into().unwrap(),
                    ExtendedColorType::Rgb16.into(),
                )?;
            }

            _ => {}
        }
    }

    Ok(())
}

// ================= METADATA =================
#[cfg(feature = "exif")]
fn copy_metadata(
    src: &Path,
    dst: &Path,
    config: &Config,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(meta_cfg) = &config.metadata {
        if meta_cfg.copy_exif.unwrap_or(false) {
            if let Ok(src_meta) = Metadata::new_from_path(src) {
                if let Ok(mut dst_meta) = Metadata::new_from_path(dst) {
                    if let Ok(exif) = src_meta.get_exif() {
                        let _ = dst_meta.set_exif(&exif);
                    }
                    let _ = dst_meta.save_to_file(dst);
                }
            }
        }
    }

    Ok(())
}

#[cfg(not(feature = "exif"))]
fn copy_metadata(
    _src: &Path,
    _dst: &Path,
    _config: &Config,
) -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}
