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

use bytemuck;
use image::{
    EncodableLayout, ExtendedColorType, ImageEncoder,
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
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

// ================= DEFAULT CONFIG =================

const DEFAULT_CONFIG: &str = include_str!("config.toml");

// ================= CONFIG =================

#[derive(Deserialize)]
struct Config {
    storage_root: String,
    outputs: Vec<OutputConfig>,
    metadata: Option<MetadataConfig>,
}

#[derive(Deserialize)]
struct OutputConfig {
    format: String,
    quality: Option<u8>,
    bit_depth: Option<u8>,
    compression: Option<String>,
    icc: Option<String>,
}

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
        process_file(f, &base, &config).unwrap();
    });

    Ok(())
}

// ================= CONFIG LOADER =================

fn load_or_create_config() -> Result<Config, Box<dyn std::error::Error>> {
    let home = dirs::home_dir().ok_or("No home directory")?;
    let cfg_dir = home.join(".nphoto");
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
    let mut raw = RawImage::open(raw_path.as_os_str().as_encoded_bytes()).unwrap();
    let img = raw.process::<16>().unwrap();

    let width = img.width();
    let height = img.height();

    let buf = img.as_bytes();
    let stem = raw_path.file_stem().unwrap().to_string_lossy();

    for out in &config.outputs {
        // ICC load
        let icc_data = if let Some(path) = &out.icc {
            Some(fs::read(path)?)
        } else {
            None
        };

        // COLOR TRANSFORM (lcms2)
        let mut buf_out = vec![0u16; buf.len()];
        if let Some(ref icc) = icc_data {
            let transform = Transform::new(
                &Profile::new_icc(icc)?,
                PixelFormat::RGB_16,
                &Profile::new_icc(icc)?,
                PixelFormat::RGB_16,
                Intent::Perceptual,
            )?;
            transform.transform_pixels(buf, &mut buf_out);
        }

        let buf8: Vec<u8> = buf.iter().map(|v| (v >> 8) as u8).collect();

        match out.format.as_str() {
            "jpeg" => {
                let path = base.join(format!("{}_{}.jpg", stem, out.quality.unwrap_or(90)));
                let mut writer = BufWriter::new(File::create(&path)?);
                let mut enc = JpegEncoder::new_with_quality(&mut writer, out.quality.unwrap_or(90));

                if let Some(ref icc) = icc_data {
                    let _ = enc.set_icc_profile(icc.clone());
                }

                enc.write_image(&buf8, width, height, ExtendedColorType::Rgb8.into())?;
                copy_metadata(raw_path, &path, config)?;
            }

            "png" => {
                let path = base.join(format!("{}.png", stem));
                let mut enc = PngEncoder::new(File::create(&path)?);

                if let Some(ref icc) = icc_data {
                    let _ = enc.set_icc_profile(icc.clone());
                }

                let raw = bytemuck::cast_slice(&buf);

                enc.write_image(raw, width, height, ExtendedColorType::Rgb16.into())?;
            }

            "tiff" => {
                use tiff::encoder::*;

                let path = base.join(format!("{}.tiff", stem));
                let mut tiff = TiffEncoder::new(File::create(&path)?).unwrap();
                let image = tiff.new_image::<colortype::RGB16>(width, height)?;
                let raw = bytemuck::cast_slice(&buf);
                image.write_data(raw)?;
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
